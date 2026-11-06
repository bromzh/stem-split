//! Owned subprocesses with streamed output and explicit cancellation.

use std::{ffi::OsStr, io, path::Path, process::Stdio, time::Duration};
use stem_split_core::CancelToken;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

use crate::{DownloadError, MediaUrl, Progress};

fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    platform::hide(&mut command);
    command
}

mod platform {
    #[cfg(windows)]
    pub(super) fn hide(command: &mut tokio::process::Command) {
        command.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    pub(super) fn hide(_command: &mut tokio::process::Command) {}
}

pub(super) async fn capture<I, S>(
    program: &Path,
    args: I,
    cancel: &CancelToken,
) -> Result<Option<Vec<u8>>, DownloadError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = command(program);
    command.args(args);
    let mut result = Vec::new();
    let completed = run(command, cancel, |stream, line| {
        if let Stream::Stdout = stream {
            result.extend_from_slice(line.as_bytes());
            result.push(b'\n');
        }
    })
    .await?;
    Ok(completed.then_some(result))
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
    run(command, cancel, |_, line| {
        if let Some(progress) = parse_progress(line) {
            report(progress);
        }
    })
    .await
}

pub(super) async fn convert(
    program: &Path,
    source: &Path,
    output: &Path,
    cancel: &CancelToken,
) -> Result<bool, DownloadError> {
    encode(
        program,
        source,
        output,
        stem_split_core::domain::OutputFormat::Wav,
        cancel,
    )
    .await
}

pub(super) async fn encode(
    program: &Path,
    source: &Path,
    output: &Path,
    format: stem_split_core::domain::OutputFormat,
    cancel: &CancelToken,
) -> Result<bool, DownloadError> {
    let mut command = command(program);
    command
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(source)
        .arg("-vn");
    match format {
        stem_split_core::domain::OutputFormat::Wav => {
            command.args(["-c:a", "pcm_f32le", "-f", "wav"]);
        }
        stem_split_core::domain::OutputFormat::Mp3 => {
            command.args(["-c:a", "libmp3lame", "-b:a", "320k", "-f", "mp3"]);
        }
    }
    // Destinations are owned staging files; user files are published separately.
    command.arg("-y").arg(output);
    run(command, cancel, |_, _| {}).await
}

enum Stream {
    Stdout,
    Stderr,
}

async fn run(
    mut command: Command,
    cancel: &CancelToken,
    mut on_line: impl FnMut(Stream, &str),
) -> Result<bool, DownloadError> {
    if cancel.is_cancelled() {
        return Ok(false);
    }
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("stderr unavailable"))?;
    let mut stdout = BufReader::new(stdout).lines();
    let mut stderr = BufReader::new(stderr).lines();
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut diagnostic = String::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    while stdout_open || stderr_open {
        tokio::select! {
            line = stdout.next_line(), if stdout_open => match line? {
                Some(line) => on_line(Stream::Stdout, &line),
                None => stdout_open = false,
            },
            line = stderr.next_line(), if stderr_open => match line? {
                Some(line) => {
                    on_line(Stream::Stderr, &line);
                    // Keep the latest diagnostic rather than unbounded logs.
                    diagnostic = line.chars().take(4096).collect();
                },
                None => stderr_open = false,
            },
            _ = tick.tick() => if cancel.is_cancelled() {
                child.kill().await?;
                child.wait().await?;
                return Ok(false);
            },
        }
    }
    let status = child.wait().await?;
    if !status.success() {
        return Err(DownloadError::Process(diagnostic));
    }
    Ok(true)
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
