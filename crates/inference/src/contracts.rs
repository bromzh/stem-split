//! Device choices and typed model execution errors.
use std::{num::NonZeroUsize, path::PathBuf, thread};
use stem_split_audio::{closed_enum, domain::Stem};
#[derive(Debug, thiserror::Error)]
pub enum StftError {
    #[error("signal of {0} samples is too short for the transform")]
    TooShort(usize),
    #[error("FFT failed: {0}")]
    Fft(#[from] realfft::FftError),
    #[error("spectrogram plane has {actual} values, expected {expected}")]
    PlaneSize { expected: usize, actual: usize },
    #[error("overlap-add buffer of {buffer} samples cannot hold {start}..{end}")]
    Range {
        buffer: usize,
        start: usize,
        end: usize,
    },
}

closed_enum! {
    /// Which device the user wants the model to run on.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum DevicePreference {
        /// The GPU if it works, otherwise the CPU.
        #[default]
        Auto,
        /// The GPU; fail if it cannot be used.
        Gpu,
        Cpu,
    }
}

/// The device a model actually runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveDevice {
    /// A GPU through `DirectML` on Windows or `CoreML` on macOS.
    DirectMl,
    Cpu,
}

/// Why a model could not be loaded or run.
#[derive(Debug, thiserror::Error)]
pub enum InferenceError {
    #[error("cannot load the model `{}`: {source}", path.display())]
    Load {
        path: PathBuf,
        #[source]
        source: Box<ort::Error>,
    },
    #[error("GPU acceleration is not available on this platform")]
    GpuUnsupported,
    #[error("cannot run the model `{}` on the GPU: {source}", path.display())]
    GpuUnavailable {
        path: PathBuf,
        #[source]
        source: Box<ort::Error>,
    },
    #[error("the model `{}` has no tensor named `{tensor}`", path.display())]
    MissingTensor { path: PathBuf, tensor: &'static str },
    #[error("the model run failed: {0}")]
    Run(#[source] Box<ort::Error>),
    #[error("tensor `{tensor}` has the unexpected shape {shape:?}")]
    OutputShape {
        tensor: &'static str,
        shape: Vec<i64>,
    },
    #[error("the model output has {available} stems, stem {index} was requested")]
    MissingStem { index: usize, available: usize },
    #[error("a graph is set up to supply the {stem:?} stem, which the model does not have")]
    StemNotInLayout { stem: Stem },
    #[error(transparent)]
    Stft(#[from] StftError),
    #[error("the model produced implausible audio on {device:?}")]
    Implausible { device: ActiveDevice },
}

impl From<ort::Error> for InferenceError {
    fn from(error: ort::Error) -> Self {
        Self::Run(Box::new(error))
    }
}

/// How many threads a CPU session may use; at least one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuThreads(NonZeroUsize);

impl CpuThreads {
    #[must_use]
    pub const fn new(threads: NonZeroUsize) -> Self {
        Self(threads)
    }

    /// Every logical core. On the machine the settings were measured on,
    /// that is about a tenth faster than ONNX Runtime's default of one
    /// thread per physical core.
    #[must_use]
    pub fn all() -> Self {
        Self(thread::available_parallelism().unwrap_or(NonZeroUsize::MIN))
    }

    /// An equal share of the cores for each of `sessions` running at once,
    /// so that together they do not ask for more threads than there are
    /// cores.
    #[must_use]
    pub fn share(sessions: NonZeroUsize) -> Self {
        Self::all().divided_by(sessions)
    }

    fn divided_by(self, sessions: NonZeroUsize) -> Self {
        Self(NonZeroUsize::new(self.0.get() / sessions.get()).unwrap_or(NonZeroUsize::MIN))
    }

    #[must_use]
    pub const fn get(self) -> NonZeroUsize {
        self.0
    }
}
