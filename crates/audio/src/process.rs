//! Owned subprocesses with streamed output and explicit cancellation.

use crate::CancelToken;
use std::{ffi::OsStr, io, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

use crate::ffmpeg::Error;

#[must_use]
pub fn command(program: &Path) -> Command {
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

/// Captures standard output while draining diagnostics and supporting cancellation.
/// # Errors
/// Returns a process or pipe I/O error.
pub async fn capture<I, S>(
    program: &Path,
    args: I,
    cancel: &CancelToken,
) -> Result<Option<Vec<u8>>, Error>
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

#[derive(Debug, Clone, Copy)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Runs and reaps a child process, delivering bounded lines to the caller.
/// # Errors
/// Returns an error if the process cannot be started, read, stopped or completed successfully.
pub async fn run(
    mut command: Command,
    cancel: &CancelToken,
    mut on_line: impl FnMut(Stream, &str),
) -> Result<bool, Error> {
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
        return Err(Error::Process(diagnostic));
    }
    Ok(true)
}
