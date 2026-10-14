//! Creating ONNX Runtime sessions for each kind of device.
//!
//! This is the only place that knows which execution providers exist on which
//! platform; everything above it works with "a GPU session, if there is one".

use std::path::Path;

use ort::session::Session;

use super::CpuThreads;

/// A session on the CPU that uses `threads` threads.
pub(super) fn cpu_session(graph: &Path, threads: CpuThreads) -> ort::Result<Session> {
    let mut builder = Session::builder()?.with_intra_threads(threads.get().get())?;
    builder.commit_from_file(graph)
}

/// A session on the GPU, or `None` where no GPU provider is built in.
#[cfg(windows)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the signature is shared with platforms that have no GPU provider"
)]
pub(super) fn gpu_session(graph: &Path) -> Option<ort::Result<Session>> {
    Some(directml_session(graph))
}

/// A session on the GPU, or `None` where no GPU provider is built in.
#[cfg(target_os = "macos")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the signature is shared with platforms that have no GPU provider"
)]
pub(super) fn gpu_session(graph: &Path) -> Option<ort::Result<Session>> {
    Some(coreml_session(graph))
}

/// A session on the GPU, or `None` where no GPU provider is built in.
#[cfg(not(any(windows, target_os = "macos")))]
pub(super) fn gpu_session(_graph: &Path) -> Option<ort::Result<Session>> {
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
        // single node, and for these models that node returns values tens of
        // thousands of times too large without reporting an error.
        .with_config_entry("ep.dml.disable_graph_fusion", "1")?
        .with_execution_providers([directml])?
        // Both required by ONNX Runtime for the DirectML provider.
        .with_memory_pattern(false)?
        .with_parallel_execution(false)?;
    builder.commit_from_file(graph)
}
