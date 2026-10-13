//! Shows what the core reports through the `log` facade.
//!
//! Warnings matter to a user who wonders why a job ran on the processor or
//! skipped part of a damaged file, so they go to standard error.

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

/// Installs the logger. Installing can only fail if another logger is
/// already in place, in which case that one keeps working.
pub(crate) fn install() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}
