//! Explicit management of cached media executables. Call from a worker thread.

use crate::{DownloadError, Tool};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use stem_split_core::CancelToken;

/// A user-requested tool operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Install,
    Update,
    Remove,
}

/// The shared cache location for a tool executable.
#[must_use]
pub fn path(cache: &Path, tool: Tool) -> PathBuf {
    cache.join(format!("{}{}", tool.name(), std::env::consts::EXE_SUFFIX))
}

/// Runs a tool operation. Updates retain the previous binary until publication.
///
/// # Errors
/// Returns installation, network or filesystem errors.
pub fn manage(
    cache: &Path,
    tool: Tool,
    action: Action,
    cancel: &CancelToken,
) -> Result<(), DownloadError> {
    match action {
        Action::Remove => match fs::remove_file(path(cache, tool)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        },
        Action::Install | Action::Update => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let installed = match action {
                Action::Install => {
                    runtime.block_on(crate::install(cache, tool, &|_| {}, cancel))?
                }
                Action::Update => {
                    runtime.block_on(crate::install_fresh(cache, tool, &|_| {}, cancel))?
                }
                Action::Remove => return Ok(()),
            };
            drop(installed);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removal_only_deletes_the_selected_tool_and_is_repeatable() -> Result<(), DownloadError> {
        let directory = tempfile::tempdir()?;
        let cache = directory.path();
        for &tool in Tool::ALL {
            fs::write(path(cache, tool), b"binary")?;
        }
        fs::write(cache.join("model.onnx"), b"model")?;
        let token = CancelToken::new();
        manage(cache, Tool::Downloader, Action::Remove, &token)?;
        manage(cache, Tool::Downloader, Action::Remove, &token)?;
        assert!(!path(cache, Tool::Downloader).exists());
        assert!(path(cache, Tool::Ffmpeg).is_file());
        assert!(cache.join("model.onnx").is_file());
        Ok(())
    }

    #[test]
    fn cancelled_update_preserves_the_installed_binary() -> Result<(), DownloadError> {
        let directory = tempfile::tempdir()?;
        let binary = path(directory.path(), Tool::Downloader);
        fs::write(&binary, b"old binary")?;
        let token = CancelToken::new();
        token.cancel();
        manage(directory.path(), Tool::Downloader, Action::Update, &token)?;
        assert_eq!(fs::read(binary)?, b"old binary");
        Ok(())
    }
}
