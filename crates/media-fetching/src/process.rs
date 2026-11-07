//! yt-dlp arguments and download progress over the shared owned-process runner.
use crate::{DownloadError, MediaUrl, Progress};
use std::{ffi::OsStr, path::Path};
use stem_split_audio::{
    CancelToken,
    process::{command, run},
};

pub(super) async fn capture<I, S>(
    program: &Path,
    args: I,
    cancel: &CancelToken,
) -> Result<Option<Vec<u8>>, DownloadError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Ok(stem_split_audio::process::capture(program, args, cancel).await?)
}

pub(super) async fn audio(
    program: &Path,
    url: &MediaUrl,
    stage: &Path,
    report: &impl Fn(Progress),
    cancel: &CancelToken,
) -> Result<bool, DownloadError> {
    let mut command = command(program);
    command.args(["--ignore-config", "--encoding", "utf-8", "--socket-timeout", "30", "--no-cache-dir", "--no-playlist", "--playlist-items", "1",
        "--no-overwrites", "--newline", "--no-colors", "--progress", "--progress-delta", "0.1",
        "--fixup", "never", "--hls-prefer-native", "--format", "bestaudio/best",
        "--progress-template", "download:PROGRESS:%(progress.downloaded_bytes)s:%(progress.total_bytes,progress.total_bytes_estimate)s",
        "--paths"]).arg(stage).args(["--output", "source.%(ext)s", "--", url.as_str()]);
    Ok(run(command, cancel, |_, line| {
        if let Some(progress) = parse_progress(line) {
            report(progress);
        }
    })
    .await?)
}

pub(super) async fn convert(
    program: &Path,
    source: &Path,
    output: &Path,
    cancel: &CancelToken,
) -> Result<bool, DownloadError> {
    Ok(stem_split_audio::ffmpeg::convert(program, source, output, cancel).await?)
}
pub(super) async fn encode(
    program: &Path,
    source: &Path,
    output: &Path,
    format: stem_split_audio::domain::OutputFormat,
    cancel: &CancelToken,
) -> Result<bool, DownloadError> {
    Ok(stem_split_audio::ffmpeg::encode(program, source, output, format, cancel).await?)
}
fn parse_progress(line: &str) -> Option<Progress> {
    let (downloaded, total) = line.strip_prefix("PROGRESS:")?.split_once(':')?;
    Some(Progress::Downloading {
        downloaded: downloaded.parse().ok()?,
        total: total.parse::<std::num::NonZeroU64>().ok(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_accepts_known_and_unknown_totals() -> Result<(), std::num::ParseIntError> {
        assert_eq!(
            parse_progress("PROGRESS:250:1000"),
            Some(Progress::Downloading {
                downloaded: 250,
                total: Some("1000".parse()?),
            })
        );
        for line in ["PROGRESS:250:NA", "PROGRESS:250:0"] {
            assert_eq!(
                parse_progress(line),
                Some(Progress::Downloading {
                    downloaded: 250,
                    total: None
                })
            );
        }
        assert_eq!(parse_progress("WARNING: download"), None);
        assert_eq!(parse_progress("PROGRESS:NA:NA"), None);
        Ok(())
    }
}
