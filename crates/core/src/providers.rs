//! Creating ONNX Runtime sessions for each kind of device, for every network
//! the application runs: the separation models and the cleanup ones.
//!
//! This is the only place that knows which execution providers exist on which
//! platform; everything above it works with "a GPU session, if there is one".

use std::path::{Path, PathBuf};

use ort::session::Session;

use crate::separate::{ActiveDevice, CpuThreads, DevicePreference, InferenceError};

/// Whether a CPU session may take over when the GPU misbehaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fallback {
    ToCpu,
    Unavailable,
}

/// A loaded graph on the device the user's preference allows, able to move
/// to the CPU when the GPU fails under [`DevicePreference::Auto`].
#[derive(Debug)]
pub(crate) struct DeviceSession {
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
    pub(crate) fn open(
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

    pub(crate) const fn session(&self) -> &Session {
        &self.session
    }

    pub(crate) const fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// The device the next run happens on.
    pub(crate) const fn device(&self) -> ActiveDevice {
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
    pub(crate) fn fall_back(&mut self, error: InferenceError) -> Result<(), InferenceError> {
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
pub(crate) fn cpu_session(graph: &Path, threads: CpuThreads) -> ort::Result<Session> {
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
