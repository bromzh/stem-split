//! Cached `FFmpeg` encoding shared by source downloads and separated outputs.

use std::{
    io,
    path::{Path, PathBuf},
};
use stem_split_core::{
    CancelToken,
    audio::{AudioEncoder, EncodeError, Encoded},
    closed_enum,
    domain::OutputFormat,
};

use crate::{DownloadError, Progress, Tool, install, process};

closed_enum! {
    /// How downloaded material is saved in the selected destination.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum SaveFormat { #[default] Original, Wav, Mp3 }
}

impl SaveFormat {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|format| format.key() == key)
    }

    pub(super) const fn encoding(self) -> Option<OutputFormat> {
        match self {
            Self::Original => None,
            Self::Wav => Some(OutputFormat::Wav),
            Self::Mp3 => Some(OutputFormat::Mp3),
        }
    }
}

#[derive(Debug)]
pub struct FfmpegEncoder {
    executable: PathBuf,
}

impl FfmpegEncoder {
    /// Installs an encoder in the model cache when needed.
    ///
    /// # Errors
    /// Returns installation or runtime setup failures; cancellation is None.
    pub fn in_cache(
        cache: &Path,
        report: impl Fn(Progress),
        cancel: &CancelToken,
    ) -> Result<Option<Self>, DownloadError> {
        std::fs::create_dir_all(cache)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(runtime
            .block_on(install(cache, Tool::Ffmpeg, &report, cancel))?
            .map(|executable| Self { executable }))
    }
}

impl AudioEncoder for FfmpegEncoder {
    fn encode_mp3(
        &self,
        source: &Path,
        destination: &Path,
        cancel: &CancelToken,
    ) -> Result<Encoded, EncodeError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let completed = runtime
            .block_on(process::encode(
                &self.executable,
                source,
                destination,
                OutputFormat::Mp3,
                cancel,
            ))
            .map_err(|error| EncodeError::Io(io::Error::other(error.to_string())))?;
        Ok(if completed {
            Encoded::Ready
        } else {
            Encoded::Cancelled
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stem_split_core::{
        audio::{WavWriter, decode},
        domain::{InputFile, OverwritePolicy, SampleRate},
    };

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error(transparent)]
        Io(#[from] io::Error),
        #[error(transparent)]
        Write(#[from] stem_split_core::audio::WriteError),
        #[error(transparent)]
        Input(#[from] stem_split_core::domain::InputFileError),
        #[error(transparent)]
        Decode(#[from] stem_split_core::audio::DecodeError),
        #[error(transparent)]
        Media(#[from] DownloadError),
    }

    #[test]
    #[ignore = "requires STEM_SPLIT_FFMPEG pointing to a cached FFmpeg executable"]
    fn cached_ffmpeg_encodes_readable_outputs() -> Result<(), TestError> {
        let executable = std::env::var_os("STEM_SPLIT_FFMPEG")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::other("cached encoder path missing"))?;
        let encoder = FfmpegEncoder { executable };
        let directory = tempfile::tempdir()?;
        let frames: Vec<_> = (0..44_100)
            .map(|index| {
                if index % 441 < 220 {
                    [0.1, 0.1]
                } else {
                    [-0.1, -0.1]
                }
            })
            .collect();
        let mp3 = directory.path().join("stem.mp3");
        let mut writer = WavWriter::create(&mp3, SampleRate::MODEL, OverwritePolicy::Refuse)?;
        writer.write(&frames)?;
        assert_eq!(
            writer.finish_as(OutputFormat::Mp3, &encoder, &CancelToken::new())?,
            Encoded::Ready
        );
        let audio = decode(&InputFile::new(&mp3)?)?;
        assert_eq!(audio.rate(), SampleRate::MODEL);
        assert!((40_000..50_000).contains(&audio.frames().as_slice().len()));
        let wav = directory.path().join("saved.wav");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        assert!(runtime.block_on(process::encode(
            &encoder.executable,
            &mp3,
            &wav,
            OutputFormat::Wav,
            &CancelToken::new()
        ))?);
        assert_eq!(decode(&InputFile::new(wav)?)?.rate(), SampleRate::MODEL);
        Ok(())
    }
}
