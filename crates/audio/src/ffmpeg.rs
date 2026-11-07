//! `FFmpeg` installation and audio conversion, independent of URL fetching.
use crate::process::{command, run};
use crate::{AudioEncoder, CancelToken, EncodeError, Encoded, domain::OutputFormat};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};
use yt_dlp::client::deps::LibraryInstaller;

/// External installer, process or filesystem failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("tool I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("cannot install an external tool: {0}")]
    Tool(#[from] yt_dlp::error::Error),
    #[error("external process failed: {0}")]
    Process(String),
}

/// The `FFmpeg` executable in the shared cache.
#[must_use]
pub fn path(cache: &Path) -> PathBuf {
    cache.join(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX))
}

/// Ensures `FFmpeg` is installed.
/// # Errors
/// Returns filesystem or installer errors.
pub async fn install(
    cache: &Path,
    report: &impl Fn(),
    cancel: &CancelToken,
) -> Result<Option<PathBuf>, Error> {
    let binary = path(cache);
    if binary.is_file() {
        return Ok(Some(binary));
    }
    match update(cache, report, cancel).await {
        Err(error) if binary.is_file() => {
            log::info!("using concurrently installed FFmpeg after: {error}");
            Ok(Some(binary))
        }
        outcome => outcome,
    }
}

/// Downloads a staged replacement, preserving the installed binary on failure.
/// # Errors
/// Returns filesystem or installer errors.
pub async fn update(
    cache: &Path,
    report: &impl Fn(),
    cancel: &CancelToken,
) -> Result<Option<PathBuf>, Error> {
    fs::create_dir_all(cache)?;
    if cancel.is_cancelled() {
        return Ok(None);
    }
    report();
    let stage = tempfile::Builder::new()
        .prefix(".media-tools-")
        .tempdir_in(cache)?;
    let installer = LibraryInstaller::new(stage.path().to_path_buf());
    let install = installer.install_ffmpeg(None);
    tokio::pin!(install);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let binary = loop {
        tokio::select! {
            result = &mut install => break result?,
            _ = tick.tick() => if cancel.is_cancelled() { return Ok(None); },
        }
    };
    if cancel.is_cancelled() {
        return Ok(None);
    }
    let target = path(cache);
    fs::rename(binary, &target)?;
    Ok(Some(target))
}
/// Converts a source into a staged float WAV.
/// # Errors
/// Returns a process or filesystem error.
pub async fn convert(
    program: &Path,
    source: &Path,
    output: &Path,
    cancel: &CancelToken,
) -> Result<bool, Error> {
    encode(
        program,
        source,
        output,
        crate::domain::OutputFormat::Wav,
        cancel,
    )
    .await
}

/// Encodes a source into a staged audio destination.
/// # Errors
/// Returns a process or filesystem error.
pub async fn encode(
    program: &Path,
    source: &Path,
    output: &Path,
    format: crate::domain::OutputFormat,
    cancel: &CancelToken,
) -> Result<bool, Error> {
    let mut command = command(program);
    command
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(source)
        .arg("-vn");
    match format {
        crate::domain::OutputFormat::Wav => {
            command.args(["-c:a", "pcm_f32le", "-f", "wav"]);
        }
        crate::domain::OutputFormat::Mp3 => {
            command.args(["-c:a", "libmp3lame", "-b:a", "320k", "-f", "mp3"]);
        }
    }
    // Destinations are owned staging files; user files are published separately.
    command.arg("-y").arg(output);
    run(command, cancel, |_, _| {}).await
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
        report: impl Fn(),
        cancel: &CancelToken,
    ) -> Result<Option<Self>, Error> {
        std::fs::create_dir_all(cache)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(runtime
            .block_on(install(cache, &report, cancel))?
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
            .block_on(encode(
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
    use crate::{
        audio::{WavWriter, decode},
        domain::{InputFile, OverwritePolicy, SampleRate},
    };

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error(transparent)]
        Io(#[from] io::Error),
        #[error(transparent)]
        Write(#[from] crate::audio::WriteError),
        #[error(transparent)]
        Input(#[from] crate::domain::InputFileError),
        #[error(transparent)]
        Decode(#[from] crate::audio::DecodeError),
        #[error(transparent)]
        Media(#[from] Error),
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
        assert!(runtime.block_on(encode(
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
