//! The application supplies compressed-audio encoding without a core dependency
//! on a downloader, executable installer, or UI framework.

use std::{io, path::Path};

use crate::CancelToken;

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("an MP3 encoder was not supplied")]
    Unavailable,
    #[error("audio encoding failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoded {
    Ready,
    Cancelled,
}

/// Encodes a finalized temporary WAV into a temporary MP3 destination.
pub trait AudioEncoder: Send + Sync {
    /// # Errors
    /// Returns an encoder setup, process, or I/O error.
    fn encode_mp3(
        &self,
        source: &Path,
        destination: &Path,
        cancel: &CancelToken,
    ) -> Result<Encoded, EncodeError>;
}

#[derive(Debug)]
pub struct WavOnly;

impl AudioEncoder for WavOnly {
    fn encode_mp3(
        &self,
        _source: &Path,
        _destination: &Path,
        _cancel: &CancelToken,
    ) -> Result<Encoded, EncodeError> {
        Err(EncodeError::Unavailable)
    }
}
