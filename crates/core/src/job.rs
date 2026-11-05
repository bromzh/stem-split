//! The whole job: from a request to stem files on disk.

use std::{
    ffi::OsString,
    iter,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use crate::{
    audio::{self, DecodeError, ResampleError, WavWriter, WriteError},
    cancel::CancelToken,
    domain::{
        InputFile, ModelId, OutputChoice, OutputDir, OverwritePolicy, SampleRate, Stem, StemLayout,
        StemSet,
    },
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

/// The files a separation job produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputKind {
    /// One WAV per model stem.
    #[default]
    Stems,
    /// A mix WAV made from selected stems, optionally accompanied by stems.
    Mix { stems: MixStems, keep_stems: bool },
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
    /// A request whose CPU sessions use every core; see
    /// [`SeparationRequest::with_cpu_threads`] for jobs that run side by side.
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
            output_kind: OutputKind::Stems,
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

    /// Output files that are already there. A caller can show them to the
    /// user before asking to start with [`OverwritePolicy::Replace`]. Only
    /// the files the job will produce are listed: a mix without kept stems
    /// does not touch the stem files.
    #[must_use]
    pub fn existing_outputs(&self) -> Vec<PathBuf> {
        let writes_stems = match self.output_kind {
            OutputKind::Stems => true,
            OutputKind::Mix { keep_stems, .. } => keep_stems,
        };
        let mut outputs = Vec::new();
        if writes_stems {
            outputs.extend(
                self.model
                    .layout()
                    .iter()
                    .map(|stem| self.stem_path(stem))
                    .filter(|path| path.exists()),
            );
        }
        if matches!(self.output_kind, OutputKind::Mix { .. }) {
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
/// The count is the one [`Progress::Separating`] will report, unless
/// resampling changes the length enough to add or drop a chunk.
#[must_use]
pub fn expected_runs(request: &SeparationRequest) -> Option<NonZeroUsize> {
    let length = audio::stated_length(&request.input)?;
    let frames = length
        .as_nanos()
        .saturating_mul(u128::from(SampleRate::MODEL.hertz().get()))
        / 1_000_000_000;
    let frames = NonZeroUsize::new(usize::try_from(frames).ok()?)?;
    // The passes the job runs: all of them when every stem is written, and
    // only the graphs of the selected stems for a bare mix of a model with
    // one graph per stem.
    let passes = request
        .model
        .spec()
        .graphs()
        .filter(|graph| match (graph.emits(), request.output_kind) {
            (_, OutputKind::Stems)
            | (StemSelection::All, OutputKind::Mix { .. })
            | (
                StemSelection::Only(_),
                OutputKind::Mix {
                    keep_stems: true, ..
                },
            ) => true,
            (StemSelection::Only(stem), OutputKind::Mix { stems, .. }) => stems.contains(stem),
        })
        .count();
    let passes = NonZeroUsize::new(passes)?;
    Some(ChunkPlan::new(frames).count().saturating_mul(passes))
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

/// The files a job will produce, claimed before any work starts so that a
/// clash is reported before anything is downloaded or computed.
struct Outputs {
    stems: Vec<StemFile>,
    mix: Option<MixFile>,
}

/// One run of a graph over the track: which stems it yields and how to get
/// the separator for it.
struct Pass<L> {
    emits: StemSelection,
    load: L,
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
    fn new() -> Self {
        Self {
            frames: Vec::new(),
            cursor: 0,
        }
    }

    /// Starts the next pass: it reports the same positions from zero.
    fn next_pass(&mut self) {
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
/// reported before anything is downloaded or computed. Then the model is
/// fetched if needed, the input decoded, and each graph of the model run over
/// the track. Files appear under their final names only at the very end.
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
    let outputs = create_outputs(request, layout)?;

    let model = match store.ensure(
        request.model,
        &mut |download| progress.report(Progress::FetchingModel(download)),
        cancel,
    )? {
        Ensured::Ready(model) => model,
        Ensured::Cancelled => return Ok(JobEnd::Cancelled),
    };
    if cancel.is_cancelled() {
        return Ok(JobEnd::Cancelled);
    }

    progress.report(Progress::Decoding);
    let audio = audio::resample(audio::decode(&request.input)?)?;
    let frames = audio.frames().as_slice();
    let frame_count = NonZeroUsize::new(frames.len()).ok_or_else(|| DecodeError::Empty {
        path: request.input.path().to_path_buf(),
    })?;

    let pass = |graph| pass_for(graph, layout, request.device, request.cpu_threads);
    separate_into(
        frames,
        &ChunkPlan::new(frame_count),
        layout,
        pass(model.first()),
        model.rest().iter().map(pass).collect(),
        outputs,
        progress,
        cancel,
    )
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
    let keep_stems = match request.output_kind {
        OutputKind::Stems => true,
        OutputKind::Mix { keep_stems, .. } => keep_stems,
    };
    let stems = if keep_stems {
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
    let mix = match request.output_kind {
        OutputKind::Stems => None,
        OutputKind::Mix { stems, .. } => {
            let path = request.mix_path();
            let writer = WavWriter::create(&path, SampleRate::MODEL, request.overwrite)?;
            Some(MixFile {
                selection: stems,
                path,
                writer,
            })
        }
    };
    Ok(Outputs { stems, mix })
}

/// Runs the contributing passes over the track and completes the files.
///
/// `outputs.stems` are in the model's stem order, so a file's position is
/// also the position of its stem in the model output. A pass that feeds
/// neither a stem file nor the mix is skipped: for a model with one graph
/// per stem, only the graphs of the selected stems run.
#[expect(
    clippy::too_many_arguments,
    reason = "the typed inputs describe one job boundary and keeping them explicit prevents invalid combinations"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the pass scheduling, output routing, cancellation, and final publishing must remain adjacent to show their resource lifetimes"
)]
fn separate_into<L, S>(
    frames: &[[f32; 2]],
    plan: &ChunkPlan,
    layout: StemLayout,
    first_pass: Pass<L>,
    other_passes: Vec<Pass<L>>,
    mut outputs: Outputs,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<JobEnd, JobError>
where
    L: FnOnce() -> Result<S, InferenceError>,
    S: Separator,
{
    let track = Track {
        frames,
        plan,
        normalization: Normalization::of(frames),
    };
    let mix = outputs.mix.as_ref().map(|mix| mix.selection);
    // The closure below runs across the loop that mutably borrows the
    // files, so it captures a copy instead.
    let file_stems: Vec<Stem> = outputs.stems.iter().map(|file| file.stem).collect();

    let contributes = |emits: StemSelection| match emits {
        StemSelection::All => true,
        StemSelection::Only(stem) => {
            file_stems.contains(&stem) || mix.is_some_and(|mix| mix.contains(stem))
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

    // With one pass the mix is streamed to its file as the frames settle.
    // With several, each adds one stem, so they are summed in memory and
    // written at the end.
    let mut accumulator = (mix.is_some() && pass_count > 1).then_some(MixAccumulator::new());
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
            return Ok(JobEnd::Cancelled);
        }
        progress.report(Progress::LoadingModel);
        let mut separator = (pass.load)()?;

        if let Some(accumulator) = &mut accumulator {
            accumulator.next_pass();
        }

        let mut targets: Vec<Target<'_>> = Vec::new();
        for (index, file) in outputs.stems.iter_mut().enumerate() {
            let emitted = match pass.emits {
                StemSelection::All => true,
                StemSelection::Only(stem) => stem == file.stem,
            };
            if emitted {
                targets.push(Target {
                    stem: index,
                    extra_stems: &[],
                    sink: &mut file.writer,
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
        if let Some((first, rest)) = mixed {
            let sink: Option<&mut dyn pipeline::FrameSink> = match accumulator.as_mut() {
                Some(accumulator) => Some(accumulator),
                // A mix always has its file; the fold to `None` exists
                // because the compiler cannot see that.
                None => outputs
                    .mix
                    .as_mut()
                    .map(|mix| &mut mix.writer as &mut dyn pipeline::FrameSink),
            };
            if let Some(sink) = sink {
                targets.push(Target {
                    stem: first,
                    extra_stems: rest,
                    sink,
                });
            }
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
            PassEnd::Cancelled => return Ok(JobEnd::Cancelled),
        }
    }

    if let (Some(accumulator), Some(mix)) = (&accumulator, &mut outputs.mix) {
        mix.writer.write(&accumulator.frames)?;
    }

    progress.report(Progress::Finalizing);
    let mut published = Vec::with_capacity(outputs.stems.len().saturating_add(1));
    for file in outputs.stems {
        file.writer.finish()?;
        published.push((file.stem, file.path));
    }
    let mix = match outputs.mix {
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
        Ok(request.with_output_kind(OutputKind::Mix { stems, keep_stems }))
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
        assert!(request.existing_outputs().is_empty());

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

        let end = separate_into(
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
        let end = separate_into(
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

        let end = separate_into(
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

        let result = separate_into(
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

        let end = separate_into(
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

        let end = separate_into(
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

        let end = separate_into(
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
        let end = separate_into(
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
        let end = separate_into(
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

        let end = separate_into(
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
}
