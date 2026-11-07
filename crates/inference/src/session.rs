//! Creating ONNX Runtime sessions for each kind of device, for every network
//! the application runs: the separation models and the cleanup ones.
//!
//! This is the only place that knows which execution providers exist on which
//! platform; everything above it works with "a GPU session, if there is one".

use std::path::{Path, PathBuf};

use ort::{
    session::{Session, SessionInputValue, SessionOutputs},
    value::TensorRef,
};

use crate::{ActiveDevice, CpuThreads, DevicePreference, InferenceError};

/// Whether a CPU session may take over when the GPU misbehaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fallback {
    ToCpu,
    Unavailable,
}

/// A loaded graph on the device the user's preference allows, able to move
/// to the CPU when the GPU fails under [`DevicePreference::Auto`].
#[derive(Debug)]
pub struct DeviceSession {
    session: Session,
    device: ActiveDevice,
    fallback: Fallback,
    graph: PathBuf,
    /// For a CPU session, including one that replaces a failed GPU.
    threads: CpuThreads,
}

impl DeviceSession {
    /// Loads `graph`. Under [`DevicePreference::Auto`] a GPU that cannot be
    /// initialized is replaced by the CPU right here; one that fails later
    /// is replaced by [`DeviceSession::fall_back`].
    ///
    /// # Errors
    ///
    /// Returns an error if no session can be created on a device the
    /// preference allows.
    pub fn open(
        graph: &Path,
        preference: DevicePreference,
        threads: CpuThreads,
    ) -> Result<Self, InferenceError> {
        let on_cpu = || {
            cpu_session(graph, threads).map_err(|source| InferenceError::Load {
                path: graph.to_path_buf(),
                source: Box::new(source),
            })
        };
        let (session, device, fallback) = match preference {
            DevicePreference::Cpu => (on_cpu()?, ActiveDevice::Cpu, Fallback::Unavailable),
            DevicePreference::Gpu => match gpu_session(graph) {
                Some(Ok(session)) => (session, ActiveDevice::DirectMl, Fallback::Unavailable),
                Some(Err(source)) => {
                    return Err(InferenceError::GpuUnavailable {
                        path: graph.to_path_buf(),
                        source: Box::new(source),
                    });
                }
                None => return Err(InferenceError::GpuUnsupported),
            },
            DevicePreference::Auto => match gpu_session(graph) {
                Some(Ok(session)) => (session, ActiveDevice::DirectMl, Fallback::ToCpu),
                Some(Err(error)) => {
                    log::warn!("GPU session could not be created, using the CPU: {error}");
                    (on_cpu()?, ActiveDevice::Cpu, Fallback::Unavailable)
                }
                None => (on_cpu()?, ActiveDevice::Cpu, Fallback::Unavailable),
            },
        };
        Ok(Self {
            session,
            device,
            fallback,
            graph: graph.to_path_buf(),
            threads,
        })
    }

    /// Whether the graph declares this input.
    #[must_use]
    pub fn has_input(&self, name: &str) -> bool {
        self.session
            .inputs()
            .iter()
            .any(|input| input.name() == name)
    }

    /// Whether the graph declares this output.
    #[must_use]
    pub fn has_output(&self, name: &str) -> bool {
        self.session
            .outputs()
            .iter()
            .any(|output| output.name() == name)
    }

    /// Executes named borrowed float tensors without exposing the runtime session.
    ///
    /// # Errors
    /// Returns a tensor-construction or graph-execution error.
    pub fn run<const N: usize>(
        &mut self,
        inputs: [TensorInput<'_>; N],
    ) -> Result<TensorOutputs<'_>, InferenceError> {
        let values = inputs
            .into_iter()
            .map(|input| {
                let tensor = TensorRef::from_array_view((input.shape.to_vec(), input.data))?;
                Ok((input.name, SessionInputValue::from(tensor)))
            })
            .collect::<Result<Vec<_>, ort::Error>>()?;
        Ok(TensorOutputs(self.session.run(values)?))
    }

    /// The device the next run happens on.
    #[must_use]
    pub const fn device(&self) -> ActiveDevice {
        self.device
    }

    /// Reacts to `error` from a run on the current device: a GPU that may be
    /// replaced is, by a CPU session, and the caller tries again. Otherwise
    /// the error is handed back.
    ///
    /// # Errors
    ///
    /// Returns `error` when there is nothing to fall back to, or the error
    /// of creating the CPU session.
    pub fn fall_back(&mut self, error: InferenceError) -> Result<(), InferenceError> {
        match (self.device, self.fallback) {
            (ActiveDevice::DirectMl, Fallback::ToCpu) => {
                log::warn!("the GPU failed, continuing on the CPU: {error}");
                self.session = cpu_session(&self.graph, self.threads).map_err(|source| {
                    InferenceError::Load {
                        path: self.graph.clone(),
                        source: Box::new(source),
                    }
                })?;
                self.device = ActiveDevice::Cpu;
                self.fallback = Fallback::Unavailable;
                Ok(())
            }
            (
                ActiveDevice::DirectMl | ActiveDevice::Cpu,
                Fallback::ToCpu | Fallback::Unavailable,
            ) => Err(error),
        }
    }
}

/// A session on the CPU that uses `threads` threads.
fn cpu_session(graph: &Path, threads: CpuThreads) -> ort::Result<Session> {
    let mut builder = Session::builder()?.with_intra_threads(threads.get().get())?;
    builder.commit_from_file(graph)
}

/// A session on the GPU, or `None` where no GPU provider is built in.
#[cfg(windows)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the signature is shared with platforms that have no GPU provider"
)]
fn gpu_session(graph: &Path) -> Option<ort::Result<Session>> {
    Some(directml_session(graph))
}

/// A session on the GPU, or `None` where no GPU provider is built in.
#[cfg(target_os = "macos")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the signature is shared with platforms that have no GPU provider"
)]
fn gpu_session(graph: &Path) -> Option<ort::Result<Session>> {
    Some(coreml_session(graph))
}

/// A session on the GPU, or `None` where no GPU provider is built in.
#[cfg(not(any(windows, target_os = "macos")))]
fn gpu_session(_graph: &Path) -> Option<ort::Result<Session>> {
    None
}

#[cfg(target_os = "macos")]
fn coreml_session(graph: &Path) -> ort::Result<Session> {
    use ort::ep::{
        self,
        coreml::{ComputeUnits, ModelFormat},
    };

    let coreml = ep::CoreML::default()
        // The GPU and the Neural Engine of Apple silicon, with the CPU for
        // whatever neither of them supports.
        .with_compute_units(ComputeUnits::All)
        // The format of Core ML 5+, the one that supports the operators of
        // transformer models such as HTDemucs.
        .with_model_format(ModelFormat::MLProgram)
        .build()
        // Without this a failed provider leaves a session that silently runs
        // on the CPU while the application reports the GPU.
        .error_on_failure();

    let mut builder = Session::builder()?.with_execution_providers([coreml])?;
    builder.commit_from_file(graph)
}

#[cfg(windows)]
fn directml_session(graph: &Path) -> ort::Result<Session> {
    use ort::ep::{self, directml::PerformancePreference};

    let directml = ep::DirectML::default()
        // Not the integrated or a virtual adapter when a discrete GPU exists.
        .with_performance_preference(PerformancePreference::HighPerformance)
        .build()
        // Without this a failed provider leaves a session that silently runs
        // on the CPU while the application reports the GPU.
        .error_on_failure();

    let mut builder = Session::builder()?
        // With every dimension static, DirectML fuses the whole graph into a
        // single node, and for the separation models that node returns values
        // tens of thousands of times too large without reporting an error.
        // The cleanup graphs get the same setting: their results were checked
        // against the CPU with it.
        .with_config_entry("ep.dml.disable_graph_fusion", "1")?
        .with_execution_providers([directml])?
        // Both required by ONNX Runtime for the DirectML provider.
        .with_memory_pattern(false)?
        .with_parallel_execution(false)?;
    builder.commit_from_file(graph)
}

/// Named, borrowed float input for a model graph.
#[derive(Debug, Clone, Copy)]
pub struct TensorInput<'a> {
    name: &'static str,
    shape: &'a [i64],
    data: &'a [f32],
}
impl<'a> TensorInput<'a> {
    #[must_use]
    pub const fn new(name: &'static str, shape: &'a [i64], data: &'a [f32]) -> Self {
        Self { name, shape, data }
    }
}

/// Borrowed model outputs. Runtime allocations live until this value is dropped.
#[derive(Debug)]
pub struct TensorOutputs<'a>(SessionOutputs<'a>);
impl TensorOutputs<'_> {
    /// Reads a float tensor and its dimensions by name.
    ///
    /// # Errors
    /// Returns an error for a missing output or a non-float tensor.
    pub fn tensor(&self, name: &'static str) -> Result<(&[i64], &[f32]), InferenceError> {
        let (shape, data) = self
            .0
            .get(name)
            .ok_or(InferenceError::OutputShape {
                tensor: name,
                shape: Vec::new(),
            })?
            .try_extract_tensor::<f32>()?;
        Ok((shape.as_ref(), data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error(transparent)]
        Io(#[from] std::io::Error),
        #[error(transparent)]
        Inference(#[from] InferenceError),
    }

    // ONNX Identity with one float [1, 2] input/output; generated locally, no external weights.
    const IDENTITY_MODEL: &[u8] = &[
        8, 9, 18, 16, 115, 116, 101, 109, 45, 115, 112, 108, 105, 116, 45, 116, 101, 115, 116, 115,
        58, 88, 10, 25, 10, 5, 105, 110, 112, 117, 116, 18, 6, 111, 117, 116, 112, 117, 116, 34, 8,
        73, 100, 101, 110, 116, 105, 116, 121, 18, 8, 105, 100, 101, 110, 116, 105, 116, 121, 90,
        23, 10, 5, 105, 110, 112, 117, 116, 18, 14, 10, 12, 8, 1, 18, 8, 10, 2, 8, 1, 10, 2, 8, 2,
        98, 24, 10, 6, 111, 117, 116, 112, 117, 116, 18, 14, 10, 12, 8, 1, 18, 8, 10, 2, 8, 1, 10,
        2, 8, 2, 66, 2, 16, 13,
    ];

    #[test]
    fn named_tensor_adapter_runs_a_cpu_graph_and_reports_invalid_tensors() -> Result<(), TestError>
    {
        let directory = tempfile::tempdir()?;
        let graph = directory.path().join("identity.onnx");
        std::fs::write(&graph, IDENTITY_MODEL)?;
        let mut session = DeviceSession::open(
            &graph,
            DevicePreference::Cpu,
            CpuThreads::new(std::num::NonZeroUsize::MIN),
        )?;
        assert!(session.has_input("input"));
        assert!(session.has_output("output"));
        assert!(!session.has_output("missing"));
        let values = [0.25, -0.5];
        let outputs = session.run([TensorInput::new("input", &[1, 2], &values)])?;
        let (shape, samples) = outputs.tensor("output")?;
        assert_eq!(shape, [1, 2]);
        assert_eq!(samples, values);
        assert!(matches!(
            outputs.tensor("missing"),
            Err(InferenceError::OutputShape {
                tensor: "missing",
                ..
            })
        ));
        drop(outputs);
        assert!(matches!(
            session.run([TensorInput::new("input", &[1, 3], &values)]),
            Err(InferenceError::Run(_))
        ));
        Ok(())
    }
}
