//! The whole job: from a request to stem files on disk.

use std::{
    ffi::OsString,
    iter,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use crate::{
    audio::{self, DecodeError, ResampleError, Spill, WavWriter, WriteError},
    cancel::CancelToken,
    domain::{
        CleanupTargets, EnhancerId, InputFile, ModelId, OutputChoice, OutputDir, OverwritePolicy,
        SampleRate, Stem, StemLayout, StemSet,
    },
    enhance::{self, Cleaned, Enhancer, MdxParams, OrtEnhancer, Prediction},
    model::{DownloadProgress, Ensured, LocalGraph, ModelStore, ModelStoreError, StemSelection},
    separate::{
        ActiveDevice, CpuThreads, DevicePreference, InferenceError, OrtSeparator, Separator,
        pipeline::{self, PassEnd, PassError, Target, Track},
        plan::{ChunkPlan, Normalization},
    },
};

/// A non-empty subset of a model's layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixStems(StemSet);

/// Why a mix selection cannot be used for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MixStemsError {
    #[error("a mix needs at least one stem")]
    Empty,
    #[error("a mix stem is not in the model layout")]
    OutsideLayout,
}

impl MixStems {
    /// Validates a selection against the model layout.
    ///
    /// # Errors
    ///
    /// Returns an error if the selection is empty or includes a stem the
    /// selected model cannot produce.
    pub fn new(stems: StemSet, layout: StemLayout) -> Result<Self, MixStemsError> {
        if stems.is_empty() {
            return Err(MixStemsError::Empty);
        }
        if !stems.is_subset_of(layout) {
            return Err(MixStemsError::OutsideLayout);
        }
        Ok(Self(stems))
    }

    /// Selected stems in layout order.
    pub fn iter(self, layout: StemLayout) -> impl Iterator<Item = Stem> {
        self.0.iter(layout)
    }

    #[must_use]
    pub const fn contains(self, stem: Stem) -> bool {
        self.0.contains(stem)
    }

    pub const fn set(self) -> StemSet {
        self.0
    }

    /// Positions of the selected stems in the model's output, in layout
    /// order. The selection is a subset of the layout, so every stem has a
    /// position.
    #[must_use]
    pub fn positions(self, layout: StemLayout) -> Vec<usize> {
        self.iter(layout)
            .filter_map(|stem| layout.position(stem))
            .collect()
    }
}

/// How a job that makes a mix cleans up after the separation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cleanup {
    pub enhancer: EnhancerId,
    pub targets: CleanupTargets,
}

/// A cleanup network was asked to clean a mix it cannot: one that tells
/// vocals from instruments, of a mix meant to have both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the `{}` network cannot clean a mix of these stems", enhancer.name())]
pub struct MixCleanupError {
    enhancer: EnhancerId,
}

/// A mix to make: of which stems, whether the stems are written as well,
/// and what is cleaned up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixOutput {
    stems: MixStems,
    keep_stems: bool,
    cleanup: Option<Cleanup>,
    /// How the cleanup treats the mix, when it cleans it. Worked out once
    /// here, so that no job asks a network to clean a mix it cannot.
    mix_prediction: Option<Prediction>,
}

impl MixOutput {
    /// # Errors
    ///
    /// Returns [`MixCleanupError`] when `cleanup` is to clean the mix and
    /// its network cannot clean a mix of `stems`; see
    /// [`MixOutput::can_clean_mix`].
    pub fn new(
        stems: MixStems,
        keep_stems: bool,
        cleanup: Option<Cleanup>,
    ) -> Result<Self, MixCleanupError> {
        let mix_prediction = match cleanup {
            Some(cleanup) if cleanup.targets.mix() => Some(
                cleanup
                    .enhancer
                    .spec()
                    .output()
                    .for_mix(stems.set())
                    .ok_or(MixCleanupError {
                        enhancer: cleanup.enhancer,
                    })?,
            ),
            Some(_) | None => None,
        };
        Ok(Self {
            stems,
            keep_stems,
            cleanup,
            mix_prediction,
        })
    }

    /// Whether `enhancer` can clean a mix of `stems`.
    #[must_use]
    pub fn can_clean_mix(enhancer: EnhancerId, stems: StemSet) -> bool {
        enhancer.spec().output().for_mix(stems).is_some()
    }

    #[must_use]
    pub const fn stems(&self) -> MixStems {
        self.stems
    }

    #[must_use]
    pub const fn keeps_stems(&self) -> bool {
        self.keep_stems
    }

    #[must_use]
    pub const fn cleanup(&self) -> Option<Cleanup> {
        self.cleanup
    }
}

/// The files a separation job produces, and what is cleaned up in them.
///
/// A job that writes only stems can clean nothing but its stems, so the
/// choice of what to clean exists only for a mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    /// One WAV per model stem, each cleaned by `cleanup` when there is one.
    Stems { cleanup: Option<EnhancerId> },
    /// A mix WAV made from selected stems, optionally accompanied by stems.
    Mix(MixOutput),
}

/// What the cleanup stage of a job works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cleaning {
    enhancer: EnhancerId,
    /// Every stem the job uses is cleaned before it is written or mixed.
    stems: bool,
    /// The mix is cleaned this way before it is written.
    mix: Option<Prediction>,
}

impl OutputKind {
    /// Stem files without cleanup: what a job makes unless told otherwise.
    pub const STEMS: Self = Self::Stems { cleanup: None };

    /// Whether a file is written for every stem of the model.
    #[must_use]
    pub const fn writes_stems(self) -> bool {
        match self {
            Self::Stems { .. } => true,
            Self::Mix(mix) => mix.keep_stems,
        }
    }

    /// The stems of the mix, when the job makes one.
    #[must_use]
    pub const fn mix(self) -> Option<MixStems> {
        match self {
            Self::Stems { .. } => None,
            Self::Mix(mix) => Some(mix.stems),
        }
    }

    /// The cleanup network the job runs, if any.
    #[must_use]
    pub fn enhancer(self) -> Option<EnhancerId> {
        self.cleaning().map(|cleaning| cleaning.enhancer)
    }

    fn cleaning(self) -> Option<Cleaning> {
        match self {
            Self::Stems { cleanup } => cleanup.map(|enhancer| Cleaning {
                enhancer,
                stems: true,
                mix: None,
            }),
            Self::Mix(mix) => mix.cleanup.map(|cleanup| Cleaning {
                enhancer: cleanup.enhancer,
                stems: cleanup.targets.stems(),
                mix: mix.mix_prediction,
            }),
        }
    }

    /// Every stem the job writes or mixes.
    fn used_stems(self, layout: StemLayout) -> StemSet {
        let mixed = self.mix().map_or(StemSet::none(), MixStems::set);
        if self.writes_stems() {
            StemSet::all(layout)
        } else {
            mixed
        }
    }
}

/// Everything a job needs to know, already checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeparationRequest {
    input: InputFile,
    output: OutputDir,
    model: ModelId,
    device: DevicePreference,
    overwrite: OverwritePolicy,
    output_kind: OutputKind,
    cpu_threads: CpuThreads,
}

impl SeparationRequest {
    /// A request for stem files without cleanup, whose CPU sessions use
    /// every core; see [`SeparationRequest::with_cpu_threads`] for jobs that
    /// run side by side and [`SeparationRequest::with_output_kind`] for
    /// other results.
    #[must_use]
    pub fn new(
        input: InputFile,
        output: &OutputChoice,
        model: ModelId,
        device: DevicePreference,
        overwrite: OverwritePolicy,
    ) -> Self {
        Self {
            output: output.resolve(&input),
            input,
            model,
            device,
            overwrite,
            output_kind: OutputKind::STEMS,
            cpu_threads: CpuThreads::all(),
        }
    }

    /// The same request with CPU sessions limited to `threads`.
    #[must_use]
    pub fn with_cpu_threads(mut self, threads: CpuThreads) -> Self {
        self.cpu_threads = threads;
        self
    }

    /// Chooses the files this request will produce.
    #[must_use]
    pub fn with_output_kind(mut self, output_kind: OutputKind) -> Self {
        self.output_kind = output_kind;
        self
    }

    #[must_use]
    pub const fn cpu_threads(&self) -> CpuThreads {
        self.cpu_threads
    }

    #[must_use]
    pub const fn input(&self) -> &InputFile {
        &self.input
    }

    #[must_use]
    pub const fn output(&self) -> &OutputDir {
        &self.output
    }

    #[must_use]
    pub const fn model(&self) -> ModelId {
        self.model
    }

    #[must_use]
    pub const fn output_kind(&self) -> OutputKind {
        self.output_kind
    }

    /// Where a stem is written: `<output>/<input name>.<stem>.wav`.
    #[must_use]
    pub fn stem_path(&self, stem: Stem) -> PathBuf {
        let mut name = OsString::from(self.input.base_name());
        name.push(".");
        name.push(stem.file_tag());
        name.push(".wav");
        self.output.path().join(name)
    }

    /// Where the selected stems are written as a mix.
    #[must_use]
    pub fn mix_path(&self) -> PathBuf {
        let mut name = OsString::from(self.input.base_name());
        name.push(".mix.wav");
        self.output.path().join(name)
    }

    /// Where a stem waits to be cleaned up: next to the file it becomes,
    /// under a name no result has. It exists only while the job runs.
    fn spill_path(&self, stem: Stem) -> PathBuf {
        let mut name = self.stem_path(stem).into_os_string();
        name.push(".raw");
        PathBuf::from(name)
    }

    /// Output files that are already there. A caller can show them to the
    /// user before asking to start with [`OverwritePolicy::Replace`]. Only
    /// the files the job will produce are listed: a mix without kept stems
    /// does not touch the stem files.
    #[must_use]
    pub fn existing_outputs(&self) -> Vec<PathBuf> {
        let mut outputs = Vec::new();
        if self.output_kind.writes_stems() {
            outputs.extend(
                self.model
                    .layout()
                    .iter()
                    .map(|stem| self.stem_path(stem))
                    .filter(|path| path.exists()),
            );
        }
        if self.output_kind.mix().is_some() {
            let mix = self.mix_path();
            if mix.exists() {
                outputs.push(mix);
            }
        }
        outputs
    }
}

/// How many model runs the job is expected to take, from the length its
/// file states, so that the time of several jobs can be estimated before
/// they start. `None` when the file does not state its length.
///
/// The count is the one [`Progress::Separating`] will report plus the
/// windows of the cleanup, unless resampling changes the length enough to
/// add or drop a chunk.
#[must_use]
pub fn expected_runs(request: &SeparationRequest) -> Option<NonZeroUsize> {
    let length = audio::stated_length(&request.input)?;
    let frames = length
        .as_nanos()
        .saturating_mul(u128::from(SampleRate::MODEL.hertz().get()))
        / 1_000_000_000;
    let frames = NonZeroUsize::new(usize::try_from(frames).ok()?)?;
    runs_for(request, frames)
}

/// The model runs of `request` for a track of `frames` frames.
fn runs_for(request: &SeparationRequest, frames: NonZeroUsize) -> Option<NonZeroUsize> {
    let layout = request.model.layout();
    let used = request.output_kind.used_stems(layout);
    // The passes the job runs: those of combined graphs, and of specialist
    // graphs whose stem it uses.
    let passes = request
        .model
        .spec()
        .graphs()
        .filter(|graph| match graph.emits() {
            StemSelection::All => true,
            StemSelection::Only(stem) => used.contains(stem),
        })
        .count();
    let passes = NonZeroUsize::new(passes)?;
    let separation = ChunkPlan::new(frames).count().saturating_mul(passes);
    let windows = |enhancer: EnhancerId| {
        enhancer
            .spec()
            .params()
            .chunking()
            .map_or(0, |chunking| chunking.windows(frames.get()))
    };
    let cleanup = request.output_kind.cleaning().map_or(0, |cleaning| {
        let stems = if cleaning.stems {
            used.iter(layout).count()
        } else {
            0
        };
        let mix = usize::from(cleaning.mix.is_some());
        windows(cleaning.enhancer).saturating_mul(stems.saturating_add(mix))
    });
    Some(separation.saturating_add(cleanup))
}

/// What a running job is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// Downloading model files that are not in the cache yet.
    FetchingModel(DownloadProgress),
    /// Reading the input file and bringing it to the model's sample rate.
    Decoding,
    /// Loading a model graph; takes a couple of seconds per graph.
    LoadingModel,
    /// `done` of `total` model runs are finished.
    Separating {
        done: usize,
        total: NonZeroUsize,
        device: ActiveDevice,
    },
    /// `done` of `total` windows of the cleanup are finished.
    Enhancing {
        done: usize,
        total: NonZeroUsize,
        device: ActiveDevice,
    },
    /// Completing the output files.
    Finalizing,
}

/// Receives progress from the thread that runs the job.
pub trait ProgressSink {
    fn report(&self, progress: Progress);
}

impl<F: Fn(Progress)> ProgressSink for F {
    fn report(&self, progress: Progress) {
        self(progress);
    }
}

/// What a completed job produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeparationOutput {
    files: Vec<(Stem, PathBuf)>,
    /// The mix of the selected stems, when the job made one.
    mix: Option<PathBuf>,
    device: ActiveDevice,
}

impl SeparationOutput {
    /// The stem files, in the model's stem order.
    #[must_use]
    pub fn files(&self) -> &[(Stem, PathBuf)] {
        &self.files
    }

    /// The mix file, when the job made one.
    #[must_use]
    pub fn mix(&self) -> Option<&Path> {
        self.mix.as_deref()
    }

    /// The device the last model run was computed on.
    #[must_use]
    pub const fn device(&self) -> ActiveDevice {
        self.device
    }
}

/// How a job ended, short of an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEnd {
    Completed(SeparationOutput),
    /// The user cancelled. No output file is left behind.
    Cancelled,
}

/// Why a job failed.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error(transparent)]
    Model(#[from] ModelStoreError),
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Resample(#[from] ResampleError),
    #[error(transparent)]
    Inference(#[from] InferenceError),
}

impl From<PassError> for JobError {
    fn from(error: PassError) -> Self {
        match error {
            PassError::Inference(error) => Self::Inference(error),
            PassError::Write(error) => Self::Write(error),
        }
    }
}

/// An output file being written.
struct StemFile {
    stem: Stem,
    path: PathBuf,
    writer: WavWriter,
}

/// The mix file being written, and which stems go into it.
struct MixFile {
    selection: MixStems,
    path: PathBuf,
    writer: WavWriter,
}

/// A stem set aside to be cleaned up.
struct SpilledStem {
    stem: Stem,
    spill: Spill,
}

/// The files a job will produce, claimed before any work starts so that a
/// clash is reported before anything is downloaded or computed, and where
/// the separation puts its stems on the way to them.
struct Outputs {
    /// Final stem files, in the model's stem order.
    stems: Vec<StemFile>,
    mix: Option<MixFile>,
    cleaning: Option<Cleaning>,
    /// When each stem is cleaned, the separation writes every used stem
    /// here instead, in the model's stem order.
    spills: Vec<SpilledStem>,
    /// Otherwise the mix is summed here: the passes of a model with one
    /// graph per stem add one stem each, and a mix that is cleaned is
    /// cleaned as a whole.
    accumulator: Option<MixAccumulator>,
}

/// One run of a graph over the track: which stems it yields and how to get
/// the separator for it.
struct Pass<L> {
    emits: StemSelection,
    load: L,
}

/// Loads the cleanup network when the cleanup stage begins.
type LoadEnhancer<'a> = Box<dyn FnOnce() -> Result<Box<dyn Enhancer + 'a>, InferenceError> + 'a>;

/// The cleanup network of a job and how it is fed.
struct Cleaner<'a> {
    params: MdxParams,
    load: LoadEnhancer<'a>,
}

/// Sums the settled frames of consecutive passes into one track-long buffer.
///
/// The passes of a multi-graph model share the chunk schedule, so each of
/// them reports the same positions and the buffer can simply add them up.
/// It grows to the length of the track; the input itself is held in memory
/// the same way.
struct MixAccumulator {
    frames: Vec<[f32; 2]>,
    /// Where the current pass appends; reset as a pass starts.
    cursor: usize,
}

impl MixAccumulator {
    const fn new() -> Self {
        Self {
            frames: Vec::new(),
            cursor: 0,
        }
    }

    /// Starts the next pass: it reports the same positions from zero.
    const fn next_pass(&mut self) {
        self.cursor = 0;
    }
}

impl pipeline::FrameSink for MixAccumulator {
    fn write(&mut self, frames: &[[f32; 2]]) -> Result<(), WriteError> {
        let end = self.cursor.saturating_add(frames.len());
        if self.frames.len() < end {
            self.frames.resize(end, [0.0; 2]);
        }
        for (held, &[left, right]) in self.frames.iter_mut().skip(self.cursor).zip(frames) {
            let [held_left, held_right] = held;
            *held_left += left;
            *held_right += right;
        }
        self.cursor = end;
        Ok(())
    }
}

/// Separates the input of `request` into stem files.
///
/// The output names are claimed first, so a clash with existing files is
/// reported before anything is downloaded or computed. Then the model and
/// the cleanup network are fetched if needed, the input decoded, each graph
/// of the model run over the track, and what the request asks for cleaned.
/// Files appear under their final names only at the very end.
///
/// `cancel` is honoured between downloaded blocks and between model runs.
///
/// # Errors
///
/// Returns the error of whichever stage failed. No output file is left
/// behind, except that files already completed stay if finishing a later one
/// fails.
pub fn run(
    request: &SeparationRequest,
    store: &ModelStore,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<JobEnd, JobError> {
    let layout = request.model.layout();
    let mut outputs = create_outputs(request, layout)?;

    let files = match store.ensure_files(
        request.model,
        request.output_kind.enhancer(),
        &mut |download| progress.report(Progress::FetchingModel(download)),
        cancel,
    )? {
        Ensured::Ready(files) => files,
        Ensured::Cancelled => return Ok(JobEnd::Cancelled),
    };
    if cancel.is_cancelled() {
        return Ok(JobEnd::Cancelled);
    }

    // The input is dropped at the end of this block, before the cleanup
    // holds stems of its own in memory.
    let (separated, length) = {
        progress.report(Progress::Decoding);
        let audio = audio::resample(audio::decode(&request.input)?)?;
        let frames = audio.frames().as_slice();
        let frame_count = NonZeroUsize::new(frames.len()).ok_or_else(|| DecodeError::Empty {
            path: request.input.path().to_path_buf(),
        })?;
        let pass = |graph| pass_for(graph, layout, request.device, request.cpu_threads);
        let separated = separate_into(
            frames,
            &ChunkPlan::new(frame_count),
            layout,
            pass(files.model().first()),
            files.model().rest().iter().map(pass).collect(),
            &mut outputs,
            progress,
            cancel,
        )?;
        (separated, frame_count)
    };
    let device = match separated {
        Separated::Completed(device) => device,
        Separated::Cancelled => return Ok(JobEnd::Cancelled),
    };

    let cleaner = files.enhancer().map(|enhancer| {
        let (path, params) = (enhancer.path(), *enhancer.params());
        let (preference, threads) = (request.device, request.cpu_threads);
        Cleaner {
            params,
            load: Box::new(move || {
                OrtEnhancer::load(path, &params, preference, threads)
                    .map(|enhancer| Box::new(enhancer) as Box<dyn Enhancer>)
            }),
        }
    });
    finish(outputs, device, length, cleaner, progress, cancel)
}

/// The pass that runs `graph` through ONNX Runtime.
fn pass_for(
    graph: &LocalGraph,
    layout: StemLayout,
    device: DevicePreference,
    threads: CpuThreads,
) -> Pass<impl FnOnce() -> Result<OrtSeparator, InferenceError>> {
    Pass {
        emits: graph.emits(),
        load: move || OrtSeparator::load(graph.path(), layout, device, threads),
    }
}

/// Claims the output names of everything the job will write: every stem
/// when they are kept, and the mix when one is requested. If a name cannot
/// be claimed, the writers created so far are dropped and release theirs.
fn create_outputs(request: &SeparationRequest, layout: StemLayout) -> Result<Outputs, WriteError> {
    let kind = request.output_kind;
    let stems = if kind.writes_stems() {
        layout
            .iter()
            .map(|stem| {
                let path = request.stem_path(stem);
                let writer = WavWriter::create(&path, SampleRate::MODEL, request.overwrite)?;
                Ok(StemFile { stem, path, writer })
            })
            .collect::<Result<_, WriteError>>()?
    } else {
        Vec::new()
    };
    let mix = match kind.mix() {
        None => None,
        Some(selection) => {
            let path = request.mix_path();
            let writer = WavWriter::create(&path, SampleRate::MODEL, request.overwrite)?;
            Some(MixFile {
                selection,
                path,
                writer,
            })
        }
    };
    let cleaning = kind.cleaning();
    let stems_cleaned = cleaning.is_some_and(|cleaning| cleaning.stems);
    let spills = if stems_cleaned {
        kind.used_stems(layout)
            .iter(layout)
            .map(|stem| {
                Ok(SpilledStem {
                    stem,
                    spill: Spill::create(request.spill_path(stem))?,
                })
            })
            .collect::<Result<_, WriteError>>()?
    } else {
        Vec::new()
    };
    // Cleaned stems are summed into the mix after the cleanup; otherwise
    // the separation sums them as it goes.
    let accumulator = if stems_cleaned {
        None
    } else {
        mix.as_ref().map(|_| MixAccumulator::new())
    };
    Ok(Outputs {
        stems,
        mix,
        cleaning,
        spills,
        accumulator,
    })
}

/// How the separation stage of a job ended, short of an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Separated {
    /// Every stem is in place; the last run was on this device.
    Completed(ActiveDevice),
    Cancelled,
}

/// Runs the contributing passes over the track into `outputs`.
///
/// `outputs.stems` are in the model's stem order, so a file's position is
/// also the position of its stem in the model output. A pass that feeds
/// nothing the job uses is skipped: for a model with one graph per stem,
/// only the graphs of the used stems run. No file is completed here.
#[expect(
    clippy::too_many_arguments,
    reason = "the typed inputs describe one job boundary and keeping them explicit prevents invalid combinations"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the pass scheduling, output routing and cancellation must remain adjacent to show their borrows"
)]
fn separate_into<L, S>(
    frames: &[[f32; 2]],
    plan: &ChunkPlan,
    layout: StemLayout,
    first_pass: Pass<L>,
    other_passes: Vec<Pass<L>>,
    outputs: &mut Outputs,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<Separated, JobError>
where
    L: FnOnce() -> Result<S, InferenceError>,
    S: Separator,
{
    let track = Track {
        frames,
        plan,
        normalization: Normalization::of(frames),
    };
    // The mix is summed during the separation unless its stems are cleaned
    // one by one first.
    let mix = outputs
        .mix
        .as_ref()
        .map(|mix| mix.selection)
        .filter(|_| outputs.accumulator.is_some());
    // Stems that are cleaned one by one go aside first, and only the
    // cleaned ones reach their files.
    let into_files = !outputs.cleaning.is_some_and(|cleaning| cleaning.stems);
    // The stems the separation writes out, as files or to be cleaned. The
    // closure below runs across the loop that mutably borrows the outputs,
    // so it captures a copy.
    let sunk: Vec<Stem> = outputs
        .stems
        .iter()
        .filter(|_| into_files)
        .map(|file| file.stem)
        .chain(outputs.spills.iter().map(|spilled| spilled.stem))
        .collect();
    let contributes = |emits: StemSelection| match emits {
        StemSelection::All => true,
        StemSelection::Only(stem) => {
            sunk.contains(&stem) || mix.is_some_and(|mix| mix.contains(stem))
        }
    };
    let pass_count = iter::once(&first_pass)
        .chain(&other_passes)
        .filter(|pass| contributes(pass.emits))
        .count();
    // A stems job runs every pass, and a mix is a non-empty selection of a
    // layout the registry covers, so a job always has a contributing pass.
    // The fallback is never reported: with no pass to run, the loop below
    // never reports either.
    let total = plan
        .count()
        .saturating_mul(NonZeroUsize::new(pass_count).unwrap_or(NonZeroUsize::MIN));

    // Positions of the mixed stems in the model output, for the pass that
    // emits them all.
    let mix_positions = mix.map(|mix| mix.positions(layout));
    let mix_split = mix_positions
        .as_deref()
        .and_then(|positions| positions.split_first());

    let mut done = 0;
    let mut device = ActiveDevice::Cpu;
    for pass in iter::once(first_pass).chain(other_passes) {
        if !contributes(pass.emits) {
            continue;
        }
        if cancel.is_cancelled() {
            return Ok(Separated::Cancelled);
        }
        progress.report(Progress::LoadingModel);
        let mut separator = (pass.load)()?;

        if let Some(accumulator) = &mut outputs.accumulator {
            accumulator.next_pass();
        }

        let emitted = |stem: Stem| match pass.emits {
            StemSelection::All => true,
            StemSelection::Only(only) => only == stem,
        };
        let mut targets: Vec<Target<'_>> = Vec::new();
        let sinks = outputs
            .stems
            .iter_mut()
            .filter(|_| into_files)
            .map(|file| (file.stem, &mut file.writer as &mut dyn pipeline::FrameSink))
            .chain(outputs.spills.iter_mut().map(|spilled| {
                (
                    spilled.stem,
                    &mut spilled.spill as &mut dyn pipeline::FrameSink,
                )
            }));
        for (stem, sink) in sinks {
            if emitted(stem) {
                let index = layout
                    .position(stem)
                    .ok_or(InferenceError::StemNotInLayout { stem })?;
                targets.push(Target {
                    stem: index,
                    extra_stems: &[],
                    sink,
                });
            }
        }
        // What the pass adds to the mix: every selected stem of a combined
        // graph, or the one stem of a specialist graph it was run for.
        let mixed: Option<(usize, &[usize])> = match pass.emits {
            StemSelection::All => mix_split.map(|(&first, rest)| (first, rest)),
            StemSelection::Only(stem) if mix.is_some_and(|mix| mix.contains(stem)) => {
                let index = layout
                    .position(stem)
                    .ok_or(InferenceError::StemNotInLayout { stem })?;
                Some((index, &[]))
            }
            StemSelection::Only(_) => None,
        };
        if let (Some((first, rest)), Some(accumulator)) = (mixed, outputs.accumulator.as_mut()) {
            targets.push(Target {
                stem: first,
                extra_stems: rest,
                sink: accumulator,
            });
        }

        let end = pipeline::run_pass(
            track,
            &mut separator,
            &mut targets,
            &mut |used| {
                done += 1;
                device = used;
                progress.report(Progress::Separating {
                    done,
                    total,
                    device,
                });
            },
            cancel,
        )?;
        match end {
            PassEnd::Completed => {}
            PassEnd::Cancelled => return Ok(Separated::Cancelled),
        }
    }
    Ok(Separated::Completed(device))
}

/// The cleanup stage once its network is loaded: how it is fed, and how far
/// it has come.
struct CleanupStage<'a> {
    params: MdxParams,
    enhancer: Box<dyn Enhancer + 'a>,
    done: usize,
    total: NonZeroUsize,
    device: ActiveDevice,
}

impl CleanupStage<'_> {
    /// Cleans `frames` as `prediction` says; `None` if the job was
    /// cancelled meanwhile.
    fn clean(
        &mut self,
        frames: &[[f32; 2]],
        prediction: Prediction,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<Option<Vec<[f32; 2]>>, InferenceError> {
        let Self {
            params,
            enhancer,
            done,
            total,
            device,
        } = self;
        let end = enhance::clean(
            frames,
            params,
            prediction,
            enhancer.as_mut(),
            &mut |used| {
                *done += 1;
                *device = used;
                progress.report(Progress::Enhancing {
                    done: *done,
                    total: *total,
                    device: used,
                });
            },
            cancel,
        )?;
        Ok(match end {
            Cleaned::Completed(cleaned) => Some(cleaned),
            Cleaned::Cancelled => None,
        })
    }
}

/// Adds `frames` into `sum`, frame by frame.
fn add_into(sum: &mut [[f32; 2]], frames: &[[f32; 2]]) {
    for (held, &[left, right]) in sum.iter_mut().zip(frames) {
        let [held_left, held_right] = held;
        *held_left += left;
        *held_right += right;
    }
}

/// Cleans what the job cleans, writes what was set aside or kept in memory,
/// and completes every file. `length` is the length of the track, which
/// every stem has; `device` is where the separation ran last.
///
/// `cleaner` is there exactly when the outputs ask for a cleanup; without
/// it, the audio set aside for one is written as it is.
fn finish(
    outputs: Outputs,
    device: ActiveDevice,
    length: NonZeroUsize,
    cleaner: Option<Cleaner<'_>>,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<JobEnd, JobError> {
    let Outputs {
        mut stems,
        mut mix,
        cleaning,
        spills,
        accumulator,
    } = outputs;

    // How the cleanup treats each stem, and the mix; `None` for what it
    // leaves alone.
    let stem_prediction = |stem: Stem| {
        cleaning
            .filter(|cleaning| cleaning.stems)
            .map(|cleaning| cleaning.enhancer.spec().output().for_stem(stem))
    };
    let mix_prediction = cleaning
        .and_then(|cleaning| cleaning.mix)
        .filter(|_| mix.is_some());
    let tracks = spills
        .len()
        .saturating_add(usize::from(mix_prediction.is_some()));
    let tracks = cleaning.and_then(|_| NonZeroUsize::new(tracks));
    let mut stage = match (tracks, cleaner) {
        (Some(tracks), Some(cleaner)) => {
            if cancel.is_cancelled() {
                return Ok(JobEnd::Cancelled);
            }
            progress.report(Progress::LoadingModel);
            let enhancer = (cleaner.load)()?;
            let windows = NonZeroUsize::new(cleaner.params.chunking()?.windows(length.get()))
                .unwrap_or(NonZeroUsize::MIN);
            Some(CleanupStage {
                params: cleaner.params,
                enhancer,
                done: 0,
                total: windows.saturating_mul(tracks),
                device,
            })
        }
        (None, _) | (Some(_), None) => None,
    };
    // Frames go through the cleanup when there is one for them, and pass
    // unchanged otherwise; `None` when the job was cancelled.
    let mut through_cleanup = |frames: Vec<[f32; 2]>,
                               prediction: Option<Prediction>|
     -> Result<Option<Vec<[f32; 2]>>, InferenceError> {
        match (&mut stage, prediction) {
            (Some(stage), Some(prediction)) => stage.clean(&frames, prediction, progress, cancel),
            (Some(_) | None, _) => Ok(Some(frames)),
        }
    };

    // Stems set aside are cleaned one at a time: the raw stem is read and
    // its file removed before the cleaned one is held next to it.
    let mut sum = (!spills.is_empty())
        .then(|| mix.as_ref().map(|_| vec![[0.0_f32; 2]; length.get()]))
        .flatten();
    for SpilledStem { stem, mut spill } in spills {
        let raw = spill.read()?;
        drop(spill);
        let Some(stem_frames) = through_cleanup(raw, stem_prediction(stem))? else {
            return Ok(JobEnd::Cancelled);
        };
        if let Some(file) = stems.iter_mut().find(|file| file.stem == stem) {
            file.writer.write(&stem_frames)?;
        }
        if let (Some(sum), Some(mix)) = (sum.as_mut(), mix.as_ref())
            && mix.selection.contains(stem)
        {
            add_into(sum, &stem_frames);
        }
    }
    // The mix, summed from the cleaned stems or during the separation, is
    // cleaned as a whole when that is asked for too.
    let mix_frames = match (sum, accumulator) {
        (Some(sum), _) => Some(sum),
        (None, Some(accumulator)) => Some(accumulator.frames),
        (None, None) => None,
    };
    if let (Some(frames), Some(mix)) = (mix_frames, mix.as_mut()) {
        let Some(frames) = through_cleanup(frames, mix_prediction)? else {
            return Ok(JobEnd::Cancelled);
        };
        mix.writer.write(&frames)?;
    }
    let device = stage.map_or(device, |stage| stage.device);

    progress.report(Progress::Finalizing);
    let mut published = Vec::with_capacity(stems.len());
    for file in stems {
        file.writer.finish()?;
        published.push((file.stem, file.path));
    }
    let mix = match mix {
        Some(mix) => {
            mix.writer.finish()?;
            Some(mix.path)
        }
        None => None,
    };
    Ok(JobEnd::Completed(SeparationOutput {
        files: published,
        mix,
        device,
    }))
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, error::Error, fs, path::Path};

    use super::*;
    use crate::{
        domain::StereoFrames,
        separate::{SEGMENT_FRAMES, testing::ScalingSeparator},
    };

    type Loader = Box<dyn FnOnce() -> Result<ScalingSeparator, InferenceError>>;

    fn loader(layout: StemLayout) -> Loader {
        Box::new(move || Ok(ScalingSeparator::new(layout.count())))
    }

    fn request_in(
        directory: &Path,
        model: ModelId,
        overwrite: OverwritePolicy,
    ) -> Result<SeparationRequest, Box<dyn Error>> {
        let input = directory.join("My Song.flac");
        fs::write(&input, b"")?;
        Ok(SeparationRequest::new(
            InputFile::new(input)?,
            &OutputChoice::BesideInput,
            model,
            DevicePreference::Cpu,
            overwrite,
        ))
    }

    /// A quiet ramp without a DC offset, so that a scaled stem is exactly
    /// the scaled input.
    #[expect(clippy::cast_precision_loss, reason = "test ramp of small integers")]
    fn frames(track: usize) -> Vec<[f32; 2]> {
        (0..track)
            .map(|index| {
                let value = ((index % 2000) as f32 - 1000.0) / 4000.0;
                [value, -value]
            })
            .collect()
    }

    /// Both stages of a job without the model files: the separation into
    /// `outputs`, then completing them, with `cleaner` for the cleanup.
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the job's own stages, whose inputs the tests vary"
    )]
    fn separate_and_cleanup<L, S>(
        track: &[[f32; 2]],
        plan: &ChunkPlan,
        layout: StemLayout,
        first: Pass<L>,
        rest: Vec<Pass<L>>,
        mut outputs: Outputs,
        cleaner: Option<Cleaner<'_>>,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<JobEnd, JobError>
    where
        L: FnOnce() -> Result<S, InferenceError>,
        S: Separator,
    {
        let length = NonZeroUsize::new(track.len()).ok_or("empty track");
        let Ok(length) = length else {
            return Ok(JobEnd::Cancelled);
        };
        match separate_into(
            track,
            plan,
            layout,
            first,
            rest,
            &mut outputs,
            progress,
            cancel,
        )? {
            Separated::Completed(device) => {
                finish(outputs, device, length, cleaner, progress, cancel)
            }
            Separated::Cancelled => Ok(JobEnd::Cancelled),
        }
    }

    /// [`separate_and_cleanup`] for jobs without a cleanup.
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the job's own stages, whose inputs the tests vary"
    )]
    fn separate_and_finish<L, S>(
        track: &[[f32; 2]],
        plan: &ChunkPlan,
        layout: StemLayout,
        first: Pass<L>,
        rest: Vec<Pass<L>>,
        outputs: Outputs,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<JobEnd, JobError>
    where
        L: FnOnce() -> Result<S, InferenceError>,
        S: Separator,
    {
        separate_and_cleanup(
            track, plan, layout, first, rest, outputs, None, progress, cancel,
        )
    }

    fn plan_for(frames: &[[f32; 2]]) -> Result<ChunkPlan, &'static str> {
        NonZeroUsize::new(frames.len())
            .map(ChunkPlan::new)
            .ok_or("empty track")
    }

    fn read(path: &Path) -> Result<StereoFrames, Box<dyn Error>> {
        let (rate, frames) = audio::decode(&InputFile::new(path)?)?.into_parts();
        assert_eq!(rate, SampleRate::MODEL);
        Ok(frames)
    }

    fn worst_error(actual: &StereoFrames, expected: &[[f32; 2]], gain: f32) -> f32 {
        actual
            .as_slice()
            .iter()
            .zip(expected)
            .flat_map(|(a, e)| a.iter().zip(e))
            .fold(0.0_f32, |worst, (a, e)| worst.max((a - e * gain).abs()))
    }

    fn names_in(directory: &Path) -> Result<Vec<String>, Box<dyn Error>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(directory)? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }

    fn mix_request(
        directory: &Path,
        model: ModelId,
        stems: StemSet,
        keep_stems: bool,
    ) -> Result<SeparationRequest, Box<dyn Error>> {
        let request = request_in(directory, model, OverwritePolicy::Refuse)?;
        let stems = MixStems::new(stems, model.layout())?;
        Ok(request.with_output_kind(OutputKind::Mix(MixOutput::new(stems, keep_stems, None)?)))
    }

    #[test]
    fn a_mix_needs_at_least_one_stem_of_the_layout() {
        assert_eq!(
            MixStems::new(StemSet::none(), StemLayout::FOUR),
            Err(MixStemsError::Empty)
        );
        assert_eq!(
            MixStems::new(StemSet::none().toggled(Stem::Piano), StemLayout::FOUR),
            Err(MixStemsError::OutsideLayout)
        );
        let selected = StemSet::none().toggled(Stem::Drums).toggled(Stem::Vocals);
        assert!(MixStems::new(selected, StemLayout::FOUR).is_ok());
    }

    #[test]
    fn stem_paths_are_named_after_the_input_and_the_stem() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let request = request_in(directory.path(), ModelId::Htdemucs, OverwritePolicy::Refuse)?;

        assert_eq!(
            request.stem_path(Stem::Vocals),
            directory.path().join("My Song.vocals.wav")
        );
        assert_eq!(
            request.stem_path(Stem::Drums),
            directory.path().join("My Song.drums.wav")
        );
        assert_eq!(request.mix_path(), directory.path().join("My Song.mix.wav"));
        Ok(())
    }

    #[test]
    fn existing_outputs_lists_only_files_that_are_there() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let request = request_in(directory.path(), ModelId::Htdemucs, OverwritePolicy::Refuse)?;
        assert_eq!(request.existing_outputs(), Vec::<PathBuf>::new());

        fs::write(request.stem_path(Stem::Bass), b"old")?;
        // A stem this model does not produce is not an output of the job.
        fs::write(request.stem_path(Stem::Piano), b"old")?;

        assert_eq!(request.existing_outputs(), [request.stem_path(Stem::Bass)]);
        Ok(())
    }

    #[test]
    fn existing_outputs_of_a_mix_depend_on_kept_stems() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let selected = StemSet::none().toggled(Stem::Vocals);
        let bare = mix_request(directory.path(), ModelId::Htdemucs, selected, false)?;
        fs::write(bare.stem_path(Stem::Bass), b"old")?;
        fs::write(bare.mix_path(), b"old")?;

        // A bare mix does not touch the stem files.
        assert_eq!(bare.existing_outputs(), [bare.mix_path()]);

        let kept = mix_request(directory.path(), ModelId::Htdemucs, selected, true)?;
        assert_eq!(
            kept.existing_outputs(),
            [kept.stem_path(Stem::Bass), kept.mix_path()]
        );
        Ok(())
    }

    #[test]
    fn single_graph_job_writes_every_stem() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let request = request_in(directory.path(), ModelId::Htdemucs, OverwritePolicy::Refuse)?;
        let layout = request.model().layout();
        let track = frames(SEGMENT_FRAMES + 50_000);
        let reports = RefCell::new(Vec::new());

        let end = separate_and_finish(
            &track,
            &plan_for(&track)?,
            layout,
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_outputs(&request, layout)?,
            &|progress| reports.borrow_mut().push(progress),
            &CancelToken::new(),
        )?;

        let JobEnd::Completed(output) = end else {
            return Err("job was cancelled".into());
        };
        assert_eq!(output.mix(), None);
        let stems: Vec<Stem> = output.files().iter().map(|&(stem, _)| stem).collect();
        assert_eq!(stems, layout.iter().collect::<Vec<_>>());
        assert_eq!(output.device(), ActiveDevice::Cpu);
        // Stem `k` of the test separator is the input times `k + 1`.
        for ((_, path), gain) in output.files().iter().zip([1.0, 2.0, 3.0, 4.0]) {
            let written = read(path)?;
            assert_eq!(written.len().get(), track.len());
            let error = worst_error(&written, &track, gain);
            assert!(error < 1e-4, "{}: error {error}", path.display());
        }
        assert_eq!(
            names_in(directory.path())?,
            [
                "My Song.bass.wav",
                "My Song.drums.wav",
                "My Song.flac",
                "My Song.other.wav",
                "My Song.vocals.wav",
            ]
        );

        let reports = reports.into_inner();
        assert_eq!(reports.first(), Some(&Progress::LoadingModel));
        assert_eq!(reports.last(), Some(&Progress::Finalizing));
        let runs: Vec<usize> = reports
            .iter()
            .filter_map(|report| match report {
                Progress::Separating { done, total, .. } => {
                    assert_eq!(total.get(), 2);
                    Some(*done)
                }
                Progress::FetchingModel(_)
                | Progress::Decoding
                | Progress::LoadingModel
                | Progress::Enhancing { .. }
                | Progress::Finalizing => None,
            })
            .collect();
        assert_eq!(runs, [1, 2]);
        Ok(())
    }

    #[test]
    fn one_graph_per_stem_job_takes_each_stem_from_its_own_pass() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let request = request_in(
            directory.path(),
            ModelId::HtdemucsFt,
            OverwritePolicy::Refuse,
        )?;
        let layout = request.model().layout();
        let track = frames(300_000);
        let loads = RefCell::new(0);
        let reports = RefCell::new(Vec::new());

        let pass = |stem| Pass {
            emits: StemSelection::Only(stem),
            load: || {
                *loads.borrow_mut() += 1;
                Ok(ScalingSeparator::new(layout.count()))
            },
        };
        let mut stems = layout.iter();
        let first = pass(stems.next().ok_or("layout without stems")?);
        let end = separate_and_finish(
            &track,
            &plan_for(&track)?,
            layout,
            first,
            stems.map(pass).collect(),
            create_outputs(&request, layout)?,
            &|progress| reports.borrow_mut().push(progress),
            &CancelToken::new(),
        )?;

        let JobEnd::Completed(output) = end else {
            return Err("job was cancelled".into());
        };
        assert_eq!(*loads.borrow(), 4);
        for ((_, path), gain) in output.files().iter().zip([1.0, 2.0, 3.0, 4.0]) {
            let written = read(path)?;
            assert_eq!(written.len().get(), track.len());
            let error = worst_error(&written, &track, gain);
            assert!(error < 1e-4, "{}: error {error}", path.display());
        }
        // Two chunks for each of the four passes.
        let last_run = reports
            .borrow()
            .iter()
            .rev()
            .find_map(|report| match report {
                Progress::Separating { done, total, .. } => Some((*done, total.get())),
                Progress::FetchingModel(_)
                | Progress::Decoding
                | Progress::LoadingModel
                | Progress::Enhancing { .. }
                | Progress::Finalizing => None,
            });
        assert_eq!(last_run, Some((8, 8)));
        Ok(())
    }

    #[test]
    fn cancelled_job_leaves_no_output_files() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let request = request_in(directory.path(), ModelId::Htdemucs, OverwritePolicy::Refuse)?;
        let layout = request.model().layout();
        let track = frames(1_500_000);
        let cancel = CancelToken::new();
        let runs = RefCell::new(0);

        let end = separate_and_finish(
            &track,
            &plan_for(&track)?,
            layout,
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_outputs(&request, layout)?,
            &|progress| {
                if let Progress::Separating { done, .. } = progress {
                    *runs.borrow_mut() = done;
                    if done == 2 {
                        cancel.cancel();
                    }
                }
            },
            &cancel,
        )?;

        assert_eq!(end, JobEnd::Cancelled);
        assert_eq!(*runs.borrow(), 2, "at most one model run after the request");
        assert_eq!(names_in(directory.path())?, ["My Song.flac"]);
        Ok(())
    }

    #[test]
    fn failed_model_load_leaves_no_output_files() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let request = request_in(directory.path(), ModelId::Htdemucs, OverwritePolicy::Refuse)?;
        let layout = request.model().layout();
        let track = frames(1000);
        let failing: Loader = Box::new(|| Err(InferenceError::GpuUnsupported));

        let result = separate_and_finish(
            &track,
            &plan_for(&track)?,
            layout,
            Pass {
                emits: StemSelection::All,
                load: failing,
            },
            Vec::new(),
            create_outputs(&request, layout)?,
            &|_| {},
            &CancelToken::new(),
        );

        assert!(matches!(
            result,
            Err(JobError::Inference(InferenceError::GpuUnsupported))
        ));
        assert_eq!(names_in(directory.path())?, ["My Song.flac"]);
        Ok(())
    }

    #[test]
    fn existing_output_stops_the_job_before_it_starts() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let request = request_in(directory.path(), ModelId::Htdemucs, OverwritePolicy::Refuse)?;
        // The third stem in model order, so that two names are claimed first.
        let clash = request.stem_path(Stem::Other);
        fs::write(&clash, b"previous result")?;

        let result = create_outputs(&request, request.model().layout()).map(|_| ());

        assert!(matches!(
            result,
            Err(WriteError::AlreadyExists { path }) if path == clash
        ));
        assert_eq!(fs::read(&clash)?, b"previous result");
        // The names claimed before the clash were released again.
        assert_eq!(
            names_in(directory.path())?,
            ["My Song.flac", "My Song.other.wav"]
        );
        Ok(())
    }

    #[test]
    fn replace_policy_overwrites_previous_results() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let request = request_in(
            directory.path(),
            ModelId::Htdemucs,
            OverwritePolicy::Replace,
        )?;
        let layout = request.model().layout();
        let previous = request.stem_path(Stem::Vocals);
        fs::write(&previous, b"previous result")?;
        let track = frames(2000);

        let end = separate_and_finish(
            &track,
            &plan_for(&track)?,
            layout,
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_outputs(&request, layout)?,
            &|_| {},
            &CancelToken::new(),
        )?;

        assert!(matches!(end, JobEnd::Completed(_)));
        assert_eq!(read(&previous)?.len().get(), track.len());
        Ok(())
    }

    #[test]
    fn mix_job_writes_the_mix_without_stem_files() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let model = ModelId::Htdemucs;
        let selected = StemSet::none().toggled(Stem::Drums).toggled(Stem::Bass);
        let request = mix_request(directory.path(), model, selected, false)?;
        let layout = model.layout();
        let track = frames(SEGMENT_FRAMES + 50_000);
        let reports = RefCell::new(Vec::new());

        let end = separate_and_finish(
            &track,
            &plan_for(&track)?,
            layout,
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_outputs(&request, layout)?,
            &|progress| reports.borrow_mut().push(progress),
            &CancelToken::new(),
        )?;

        let JobEnd::Completed(output) = end else {
            return Err("job was cancelled".into());
        };
        assert!(output.files().is_empty());
        let mix = output.mix().ok_or("no mix file")?;
        let written = read(mix)?;
        assert_eq!(written.len().get(), track.len());
        // Drums and bass of the test separator are the input times 1 and 2.
        let error = worst_error(&written, &track, 3.0);
        assert!(error < 1e-4, "mix error {error}");
        assert_eq!(
            names_in(directory.path())?,
            ["My Song.flac", "My Song.mix.wav"]
        );
        // The single pass is reported as all the work there is.
        let last_run = reports
            .borrow()
            .iter()
            .rev()
            .find_map(|report| match report {
                Progress::Separating { done, total, .. } => Some((*done, total.get())),
                Progress::FetchingModel(_)
                | Progress::Decoding
                | Progress::LoadingModel
                | Progress::Enhancing { .. }
                | Progress::Finalizing => None,
            });
        assert_eq!(last_run, Some((2, 2)));
        Ok(())
    }

    #[test]
    fn mix_job_with_kept_stems_writes_both() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let model = ModelId::Htdemucs;
        let selected = StemSet::none().toggled(Stem::Vocals);
        let request = mix_request(directory.path(), model, selected, true)?;
        let layout = model.layout();
        let track = frames(300_000);

        let end = separate_and_finish(
            &track,
            &plan_for(&track)?,
            layout,
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_outputs(&request, layout)?,
            &|_| {},
            &CancelToken::new(),
        )?;

        let JobEnd::Completed(output) = end else {
            return Err("job was cancelled".into());
        };
        assert_eq!(output.files().len(), 4);
        // Vocals of the test separator is the input times 4.
        let mix = read(output.mix().ok_or("no mix file")?)?;
        assert!(worst_error(&mix, &track, 4.0) < 1e-4);
        for ((_, path), gain) in output.files().iter().zip([1.0, 2.0, 3.0, 4.0]) {
            assert!(worst_error(&read(path)?, &track, gain) < 1e-4);
        }
        Ok(())
    }

    #[test]
    fn one_graph_per_stem_mix_skips_unselected_passes() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let model = ModelId::HtdemucsFt;
        let selected = StemSet::none().toggled(Stem::Bass).toggled(Stem::Vocals);
        let request = mix_request(directory.path(), model, selected, false)?;
        let layout = model.layout();
        let track = frames(300_000);
        let loads = RefCell::new(0);

        let pass = |stem| Pass {
            emits: StemSelection::Only(stem),
            load: || {
                *loads.borrow_mut() += 1;
                Ok(ScalingSeparator::new(layout.count()))
            },
        };
        let mut stems = layout.iter();
        let first = pass(stems.next().ok_or("layout without stems")?);
        let end = separate_and_finish(
            &track,
            &plan_for(&track)?,
            layout,
            first,
            stems.map(pass).collect(),
            create_outputs(&request, layout)?,
            &|_| {},
            &CancelToken::new(),
        )?;

        let JobEnd::Completed(output) = end else {
            return Err("job was cancelled".into());
        };
        // Only the passes of bass and vocals ran.
        assert_eq!(*loads.borrow(), 2);
        assert!(output.files().is_empty());
        let mix = read(output.mix().ok_or("no mix file")?)?;
        assert_eq!(mix.len().get(), track.len());
        // Bass and vocals are the input times 2 and 4.
        assert!(worst_error(&mix, &track, 6.0) < 1e-4);
        assert_eq!(
            names_in(directory.path())?,
            ["My Song.flac", "My Song.mix.wav"]
        );
        Ok(())
    }

    #[test]
    fn one_graph_per_stem_mix_with_kept_stems_runs_every_pass() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let model = ModelId::HtdemucsFt;
        let selected = StemSet::none().toggled(Stem::Drums);
        let request = mix_request(directory.path(), model, selected, true)?;
        let layout = model.layout();
        let track = frames(300_000);
        let loads = RefCell::new(0);

        let pass = |stem| Pass {
            emits: StemSelection::Only(stem),
            load: || {
                *loads.borrow_mut() += 1;
                Ok(ScalingSeparator::new(layout.count()))
            },
        };
        let mut stems = layout.iter();
        let first = pass(stems.next().ok_or("layout without stems")?);
        let end = separate_and_finish(
            &track,
            &plan_for(&track)?,
            layout,
            first,
            stems.map(pass).collect(),
            create_outputs(&request, layout)?,
            &|_| {},
            &CancelToken::new(),
        )?;

        let JobEnd::Completed(output) = end else {
            return Err("job was cancelled".into());
        };
        assert_eq!(*loads.borrow(), 4);
        assert_eq!(output.files().len(), 4);
        // Drums alone are the input times 1.
        let mix = read(output.mix().ok_or("no mix file")?)?;
        assert!(worst_error(&mix, &track, 1.0) < 1e-4);
        Ok(())
    }

    #[test]
    fn cancelled_mix_job_leaves_no_output_files() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let model = ModelId::Htdemucs;
        let selected = StemSet::none().toggled(Stem::Drums).toggled(Stem::Bass);
        let request = mix_request(directory.path(), model, selected, false)?;
        let layout = model.layout();
        let track = frames(1_500_000);
        let cancel = CancelToken::new();

        let end = separate_and_finish(
            &track,
            &plan_for(&track)?,
            layout,
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_outputs(&request, layout)?,
            &|progress| {
                if let Progress::Separating { done: 2, .. } = progress {
                    cancel.cancel();
                }
            },
            &cancel,
        )?;

        assert_eq!(end, JobEnd::Cancelled);
        assert_eq!(names_in(directory.path())?, ["My Song.flac"]);
        Ok(())
    }

    /// A small cleanup window, so that the tests run quickly: 16 304 frames
    /// a window.
    const TEST_CLEANUP: MdxParams = MdxParams::new(64, 16, 32, 1024, 1.0);

    /// A cleaner whose network halves what it is given.
    fn halving_cleaner<'a>() -> Cleaner<'a> {
        scaling_cleaner(0.5)
    }

    /// A cleaner whose network outputs what it is given times `gain`.
    fn scaling_cleaner<'a>(gain: f32) -> Cleaner<'a> {
        Cleaner {
            params: TEST_CLEANUP,
            load: Box::new(move || {
                Ok(
                    Box::new(crate::enhance::testing::ScalingEnhancer::new(gain))
                        as Box<dyn Enhancer>,
                )
            }),
        }
    }

    fn cleanup_windows(reports: &[Progress]) -> Vec<(usize, usize)> {
        reports
            .iter()
            .filter_map(|report| match report {
                Progress::Enhancing { done, total, .. } => Some((*done, total.get())),
                Progress::FetchingModel(_)
                | Progress::Decoding
                | Progress::LoadingModel
                | Progress::Separating { .. }
                | Progress::Finalizing => None,
            })
            .collect()
    }

    #[test]
    fn stems_job_with_cleanup_cleans_every_stem() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let model = ModelId::Htdemucs;
        let request = request_in(directory.path(), model, OverwritePolicy::Refuse)?
            .with_output_kind(OutputKind::Stems {
                cleanup: Some(EnhancerId::ReverbHq),
            });
        let layout = model.layout();
        let track = frames(20_000);
        let reports = RefCell::new(Vec::new());

        let end = separate_and_cleanup(
            &track,
            &plan_for(&track)?,
            layout,
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_outputs(&request, layout)?,
            Some(halving_cleaner()),
            &|progress| reports.borrow_mut().push(progress),
            &CancelToken::new(),
        )?;

        let JobEnd::Completed(output) = end else {
            return Err("job was cancelled".into());
        };
        // Stem `k` is the input times `k + 1`, halved by the cleanup.
        for ((_, path), gain) in output.files().iter().zip([0.5, 1.0, 1.5, 2.0]) {
            let written = read(path)?;
            assert_eq!(written.len().get(), track.len());
            assert!(
                worst_error(&written, &track, gain) < 1e-4,
                "{}",
                path.display()
            );
        }
        // The stems set aside for the cleanup are gone.
        assert_eq!(
            names_in(directory.path())?,
            [
                "My Song.bass.wav",
                "My Song.drums.wav",
                "My Song.flac",
                "My Song.other.wav",
                "My Song.vocals.wav",
            ]
        );
        // Two windows for each of the four stems, after the separation.
        let reports = reports.into_inner();
        let windows = cleanup_windows(&reports);
        assert_eq!(windows.len(), 8);
        assert_eq!(windows.last(), Some(&(8, 8)));
        assert_eq!(reports.last(), Some(&Progress::Finalizing));
        Ok(())
    }

    #[test]
    fn mix_of_cleaned_stems_writes_no_stem_files() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let model = ModelId::Htdemucs;
        let layout = model.layout();
        let selected = StemSet::none().toggled(Stem::Drums).toggled(Stem::Bass);
        let request = request_in(directory.path(), model, OverwritePolicy::Refuse)?
            .with_output_kind(OutputKind::Mix(MixOutput::new(
                MixStems::new(selected, layout)?,
                false,
                Some(Cleanup {
                    enhancer: EnhancerId::ReverbHq,
                    targets: CleanupTargets::Stems,
                }),
            )?));
        let track = frames(20_000);
        let reports = RefCell::new(Vec::new());

        let end = separate_and_cleanup(
            &track,
            &plan_for(&track)?,
            layout,
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_outputs(&request, layout)?,
            Some(halving_cleaner()),
            &|progress| reports.borrow_mut().push(progress),
            &CancelToken::new(),
        )?;

        let JobEnd::Completed(output) = end else {
            return Err("job was cancelled".into());
        };
        assert!(output.files().is_empty());
        // Drums and bass are the input times 1 and 2, each halved.
        let mix = read(output.mix().ok_or("no mix file")?)?;
        assert!(worst_error(&mix, &track, 1.5) < 1e-4);
        assert_eq!(
            names_in(directory.path())?,
            ["My Song.flac", "My Song.mix.wav"]
        );
        // Only the two mixed stems were cleaned.
        assert_eq!(cleanup_windows(&reports.into_inner()).last(), Some(&(4, 4)));
        Ok(())
    }

    #[test]
    fn mix_target_cleans_the_mix_and_keeps_the_stems_raw() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let model = ModelId::HtdemucsFt;
        let layout = model.layout();
        let selected = StemSet::none().toggled(Stem::Other).toggled(Stem::Vocals);
        let request = request_in(directory.path(), model, OverwritePolicy::Refuse)?
            .with_output_kind(OutputKind::Mix(MixOutput::new(
                MixStems::new(selected, layout)?,
                true,
                Some(Cleanup {
                    enhancer: EnhancerId::ReverbHq,
                    targets: CleanupTargets::Mix,
                }),
            )?));
        let track = frames(20_000);
        let reports = RefCell::new(Vec::new());
        let pass = |stem| Pass {
            emits: StemSelection::Only(stem),
            load: || Ok(ScalingSeparator::new(layout.count())),
        };
        let mut stems = layout.iter();
        let first = pass(stems.next().ok_or("layout without stems")?);

        let end = separate_and_cleanup(
            &track,
            &plan_for(&track)?,
            layout,
            first,
            stems.map(pass).collect(),
            create_outputs(&request, layout)?,
            Some(halving_cleaner()),
            &|progress| reports.borrow_mut().push(progress),
            &CancelToken::new(),
        )?;

        let JobEnd::Completed(output) = end else {
            return Err("job was cancelled".into());
        };
        for ((_, path), gain) in output.files().iter().zip([1.0, 2.0, 3.0, 4.0]) {
            assert!(
                worst_error(&read(path)?, &track, gain) < 1e-4,
                "{}",
                path.display()
            );
        }
        // Other and vocals are the input times 3 and 4; the mix is halved.
        let mix = read(output.mix().ok_or("no mix file")?)?;
        assert!(worst_error(&mix, &track, 3.5) < 1e-4);
        assert_eq!(cleanup_windows(&reports.into_inner()).last(), Some(&(2, 2)));
        Ok(())
    }

    #[test]
    fn cancelling_during_the_cleanup_leaves_no_files() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let model = ModelId::Htdemucs;
        let layout = model.layout();
        let request = request_in(directory.path(), model, OverwritePolicy::Refuse)?
            .with_output_kind(OutputKind::Stems {
                cleanup: Some(EnhancerId::ReverbHq),
            });
        let track = frames(40_000);
        let cancel = CancelToken::new();

        let end = separate_and_cleanup(
            &track,
            &plan_for(&track)?,
            layout,
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_outputs(&request, layout)?,
            Some(halving_cleaner()),
            &|progress| {
                if let Progress::Enhancing { done: 3, .. } = progress {
                    cancel.cancel();
                }
            },
            &cancel,
        )?;

        assert_eq!(end, JobEnd::Cancelled);
        assert_eq!(names_in(directory.path())?, ["My Song.flac"]);
        Ok(())
    }

    #[test]
    fn expected_runs_count_the_cleanup_windows() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let model = ModelId::Htdemucs;
        let layout = model.layout();
        // A minute: 8 chunks of the separation, 6 windows of the cleanup.
        let frames = NonZeroUsize::new(60 * 44_100).ok_or("empty")?;
        let request = request_in(directory.path(), model, OverwritePolicy::Refuse)?;
        let mix = MixStems::new(StemSet::none().toggled(Stem::Vocals), layout)?;
        let cleanup = |targets| {
            Some(Cleanup {
                enhancer: EnhancerId::ReverbHq,
                targets,
            })
        };
        let runs =
            |kind| runs_for(&request.clone().with_output_kind(kind), frames).map(NonZeroUsize::get);

        assert_eq!(runs(OutputKind::STEMS), Some(11));
        assert_eq!(
            runs(OutputKind::Stems {
                cleanup: Some(EnhancerId::ReverbHq)
            }),
            Some(11 + 4 * 6)
        );
        assert_eq!(
            runs(OutputKind::Mix(MixOutput::new(
                mix,
                false,
                cleanup(CleanupTargets::Stems),
            )?)),
            Some(11 + 6)
        );
        assert_eq!(
            runs(OutputKind::Mix(MixOutput::new(
                mix,
                true,
                cleanup(CleanupTargets::Mix),
            )?)),
            Some(11 + 6)
        );
        // The vocals and then the mix of them.
        assert_eq!(
            runs(OutputKind::Mix(MixOutput::new(
                mix,
                false,
                cleanup(CleanupTargets::Both),
            )?)),
            Some(11 + 2 * 6)
        );
        Ok(())
    }

    #[test]
    fn both_targets_clean_the_stems_and_then_the_mix() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let model = ModelId::Htdemucs;
        let layout = model.layout();
        let selected = StemSet::none().toggled(Stem::Drums).toggled(Stem::Bass);
        let request = request_in(directory.path(), model, OverwritePolicy::Refuse)?
            .with_output_kind(OutputKind::Mix(MixOutput::new(
                MixStems::new(selected, layout)?,
                false,
                Some(Cleanup {
                    enhancer: EnhancerId::ReverbHq,
                    targets: CleanupTargets::Both,
                }),
            )?));
        let track = frames(20_000);
        let reports = RefCell::new(Vec::new());

        let end = separate_and_cleanup(
            &track,
            &plan_for(&track)?,
            layout,
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_outputs(&request, layout)?,
            Some(halving_cleaner()),
            &|progress| reports.borrow_mut().push(progress),
            &CancelToken::new(),
        )?;

        let JobEnd::Completed(output) = end else {
            return Err("job was cancelled".into());
        };
        // Drums and bass, the input times 1 and 2, halved each, then the
        // mix of them halved again.
        let mix = read(output.mix().ok_or("no mix file")?)?;
        assert!(worst_error(&mix, &track, 0.75) < 1e-4);
        // Two windows for each of the two stems and for the mix.
        assert_eq!(cleanup_windows(&reports.into_inner()).last(), Some(&(6, 6)));
        assert_eq!(
            names_in(directory.path())?,
            ["My Song.flac", "My Song.mix.wav"]
        );
        Ok(())
    }

    #[test]
    fn vocal_network_keeps_the_vocals_and_removes_them_elsewhere() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let model = ModelId::Htdemucs;
        let layout = model.layout();
        let request = request_in(directory.path(), model, OverwritePolicy::Refuse)?
            .with_output_kind(OutputKind::Stems {
                cleanup: Some(EnhancerId::VocalBleed),
            });
        let track = frames(20_000);

        let end = separate_and_cleanup(
            &track,
            &plan_for(&track)?,
            layout,
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_outputs(&request, layout)?,
            Some(scaling_cleaner(0.25)),
            &|_| {},
            &CancelToken::new(),
        )?;

        let JobEnd::Completed(output) = end else {
            return Err("job was cancelled".into());
        };
        // The network outputs a quarter of each stem as "the vocals". In the
        // vocals, the input times 4, that quarter is what remains; from the
        // other stems it is taken away, leaving three quarters.
        for ((stem, path), gain) in output.files().iter().zip([0.75, 1.5, 2.25, 1.0]) {
            let written = read(path)?;
            assert!(worst_error(&written, &track, gain) < 1e-4, "{stem:?}");
        }
        Ok(())
    }

    #[test]
    fn vocal_network_cannot_clean_a_mix_of_vocals_and_instruments() -> Result<(), Box<dyn Error>> {
        let layout = StemLayout::FOUR;
        let mixed = MixStems::new(
            StemSet::none().toggled(Stem::Drums).toggled(Stem::Vocals),
            layout,
        )?;
        let instruments = MixStems::new(StemSet::none().toggled(Stem::Drums), layout)?;
        let cleanup = |targets| {
            Some(Cleanup {
                enhancer: EnhancerId::VocalBleed,
                targets,
            })
        };

        for targets in [CleanupTargets::Mix, CleanupTargets::Both] {
            assert!(MixOutput::new(mixed, false, cleanup(targets)).is_err());
            assert!(MixOutput::new(instruments, false, cleanup(targets)).is_ok());
        }
        // Its stems can be cleaned one by one, whatever the mix.
        assert!(MixOutput::new(mixed, false, cleanup(CleanupTargets::Stems)).is_ok());
        assert!(!MixOutput::can_clean_mix(
            EnhancerId::VocalBleed,
            mixed.set()
        ));
        assert!(MixOutput::can_clean_mix(EnhancerId::ReverbHq, mixed.set()));
        Ok(())
    }
}
