//! The whole job: from a request to stem files on disk.

use std::{ffi::OsString, iter, num::NonZeroUsize, path::PathBuf};

use crate::{
    audio::{self, DecodeError, ResampleError, WavWriter, WriteError},
    cancel::CancelToken,
    domain::{
        InputFile, ModelId, OutputChoice, OutputDir, OverwritePolicy, SampleRate, Stem, StemLayout,
    },
    model::{DownloadProgress, Ensured, LocalGraph, ModelStore, ModelStoreError, StemSelection},
    separate::{
        ActiveDevice, CpuThreads, DevicePreference, InferenceError, OrtSeparator, Separator,
        pipeline::{self, PassEnd, PassError, Target, Track},
        plan::{ChunkPlan, Normalization},
    },
};

/// Everything a job needs to know, already checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeparationRequest {
    input: InputFile,
    output: OutputDir,
    model: ModelId,
    device: DevicePreference,
    overwrite: OverwritePolicy,
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
            cpu_threads: CpuThreads::all(),
        }
    }

    /// The same request with CPU sessions limited to `threads`.
    #[must_use]
    pub fn with_cpu_threads(mut self, threads: CpuThreads) -> Self {
        self.cpu_threads = threads;
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

    /// Where a stem is written: `<output>/<input name>.<stem>.wav`.
    #[must_use]
    pub fn stem_path(&self, stem: Stem) -> PathBuf {
        let mut name = OsString::from(self.input.base_name());
        name.push(".");
        name.push(stem.file_tag());
        name.push(".wav");
        self.output.path().join(name)
    }

    /// Output files that are already there. A caller can show them to the
    /// user before asking to start with [`OverwritePolicy::Replace`].
    #[must_use]
    pub fn existing_outputs(&self) -> Vec<PathBuf> {
        self.model
            .layout()
            .iter()
            .map(|stem| self.stem_path(stem))
            .filter(|path| path.exists())
            .collect()
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
    let passes = NonZeroUsize::new(request.model.spec().graphs().count())?;
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
    device: ActiveDevice,
}

impl SeparationOutput {
    /// The stem files, in the model's stem order.
    #[must_use]
    pub fn files(&self) -> &[(Stem, PathBuf)] {
        &self.files
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

/// One run of a graph over the track: which stems it yields and how to get
/// the separator for it.
struct Pass<L> {
    emits: StemSelection,
    load: L,
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
    let files = create_files(request, layout)?;

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
        pass(model.first()),
        model.rest().iter().map(pass).collect(),
        files,
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

/// Claims the output name of every stem. If one cannot be claimed, the
/// writers created so far are dropped and release theirs.
fn create_files(
    request: &SeparationRequest,
    layout: StemLayout,
) -> Result<Vec<StemFile>, WriteError> {
    layout
        .iter()
        .map(|stem| {
            let path = request.stem_path(stem);
            let writer = WavWriter::create(&path, SampleRate::MODEL, request.overwrite)?;
            Ok(StemFile { stem, path, writer })
        })
        .collect()
}

/// Runs every pass over the track and completes the files.
///
/// There is always a first pass, as a model always has a first graph.
/// `files` are in the model's stem order, so a file's position is also the
/// position of its stem in the model output.
fn separate_into<L, S>(
    frames: &[[f32; 2]],
    plan: &ChunkPlan,
    first_pass: Pass<L>,
    other_passes: Vec<Pass<L>>,
    mut files: Vec<StemFile>,
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
    let pass_count = NonZeroUsize::MIN.saturating_add(other_passes.len());
    let total = plan.count().saturating_mul(pass_count);

    let mut done = 0;
    let mut device = ActiveDevice::Cpu;
    for pass in iter::once(first_pass).chain(other_passes) {
        if cancel.is_cancelled() {
            return Ok(JobEnd::Cancelled);
        }
        if let StemSelection::Only(stem) = pass.emits
            && !files.iter().any(|file| file.stem == stem)
        {
            return Err(InferenceError::StemNotInLayout { stem }.into());
        }
        progress.report(Progress::LoadingModel);
        let mut separator = (pass.load)()?;

        let mut targets: Vec<Target<'_>> = files
            .iter_mut()
            .enumerate()
            .filter(|(_, file)| match pass.emits {
                StemSelection::All => true,
                StemSelection::Only(stem) => file.stem == stem,
            })
            .map(|(stem_index, file)| Target {
                stem_index,
                sink: &mut file.writer,
            })
            .collect();

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

    progress.report(Progress::Finalizing);
    let mut published = Vec::with_capacity(files.len());
    for file in files {
        file.writer.finish()?;
        published.push((file.stem, file.path));
    }
    Ok(JobEnd::Completed(SeparationOutput {
        files: published,
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
    fn single_graph_job_writes_every_stem() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let request = request_in(directory.path(), ModelId::Htdemucs, OverwritePolicy::Refuse)?;
        let layout = request.model().layout();
        let track = frames(SEGMENT_FRAMES + 50_000);
        let reports = RefCell::new(Vec::new());

        let end = separate_into(
            &track,
            &plan_for(&track)?,
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_files(&request, layout)?,
            &|progress| reports.borrow_mut().push(progress),
            &CancelToken::new(),
        )?;

        let JobEnd::Completed(output) = end else {
            return Err("job was cancelled".into());
        };
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
            first,
            stems.map(pass).collect(),
            create_files(&request, layout)?,
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
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_files(&request, layout)?,
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
            Pass {
                emits: StemSelection::All,
                load: failing,
            },
            Vec::new(),
            create_files(&request, layout)?,
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

        let result = create_files(&request, request.model().layout());

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
            Pass {
                emits: StemSelection::All,
                load: loader(layout),
            },
            Vec::new(),
            create_files(&request, layout)?,
            &|_| {},
            &CancelToken::new(),
        )?;

        assert!(matches!(end, JobEnd::Completed(_)));
        assert_eq!(read(&previous)?.len().get(), track.len());
        Ok(())
    }
}
