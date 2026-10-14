//! Keeps what the core reports through the `log` facade.
//!
//! Warnings explain things a user may wonder about: why a job ran on the
//! processor, or that part of a damaged file was skipped. A release build has
//! no console to show them in, so they are also written to a file, which is
//! started afresh on every launch.

use std::{
    fs::{self, File},
    io::{self, Write},
    panic,
    path::PathBuf,
    sync::OnceLock,
};

use directories::ProjectDirs;

use crate::APP_ID;

const FILE_NAME: &str = "stem-split.log";

struct Logger {
    /// Absent if the file could not be created; standard error still works.
    file: Option<File>,
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!("[{}] {}\n", record.level(), record.args());
        // A log that cannot be written has nowhere to say so.
        match io::stderr().write_all(line.as_bytes()) {
            Ok(()) | Err(_) => {}
        }
        if let Some(mut file) = self.file.as_ref() {
            match file.write_all(line.as_bytes()) {
                Ok(()) | Err(_) => {}
            }
        }
    }

    fn flush(&self) {}
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Where the log of the current launch is kept, if the system offers a place
/// for application data.
pub(crate) fn file_path() -> Option<PathBuf> {
    let directories = ProjectDirs::from("", "", APP_ID)?;
    Some(directories.data_local_dir().join(FILE_NAME))
}

fn open_file() -> Option<File> {
    let path = file_path()?;
    fs::create_dir_all(path.parent()?).ok()?;
    File::create(path).ok()
}

/// Starts logging and makes a crash leave a trace in the log.
///
/// Installing can only fail if another logger is already in place, in which
/// case that one keeps working.
pub(crate) fn install() {
    let logger = LOGGER.get_or_init(|| Logger { file: open_file() });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
    // The application itself does not panic, but a library it uses might.
    // Without a console the message would otherwise be lost.
    panic::set_hook(Box::new(|info| {
        log::error!("the application stopped unexpectedly: {info}");
    }));
}
