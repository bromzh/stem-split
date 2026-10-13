//! Runs a whole separation job from the command line: the same code path the
//! application uses, without the window.
//!
//! ```sh
//! cargo run --release -p stem-split-core --example separate -- song.flac \
//!     --model htdemucs --device auto --output stems --replace
//! ```

use std::{cell::Cell, env, path::PathBuf, process::ExitCode, time::Instant};

use stem_split_core::{
    CancelToken,
    domain::{
        InputFile, InputFileError, ModelId, OutputChoice, OutputDir, OutputDirError,
        OverwritePolicy,
    },
    job::{self, JobEnd, JobError, Progress, SeparationRequest},
    model::{ModelStore, ModelStoreError},
    separate::DevicePreference,
};

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(
        "usage: separate <input> [--model NAME] [--device auto|gpu|cpu] [--output DIR] [--replace]"
    )]
    Usage,
    #[error("unknown model `{0}`")]
    UnknownModel(String),
    #[error("unknown device `{0}`")]
    UnknownDevice(String),
    #[error(transparent)]
    Input(#[from] InputFileError),
    #[error(transparent)]
    Output(#[from] OutputDirError),
    #[error(transparent)]
    Store(#[from] ModelStoreError),
    #[error(transparent)]
    Job(#[from] JobError),
}

/// Prints what the core reports through the `log` facade.
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

fn parse(mut arguments: impl Iterator<Item = String>) -> Result<SeparationRequest, Error> {
    let input = InputFile::new(arguments.next().ok_or(Error::Usage)?)?;
    let mut model = ModelId::default();
    let mut device = DevicePreference::default();
    let mut output = OutputChoice::default();
    let mut overwrite = OverwritePolicy::default();

    while let Some(flag) = arguments.next() {
        match flag.as_str() {
            "--model" => {
                let name = arguments.next().ok_or(Error::Usage)?;
                model = ModelId::ALL
                    .iter()
                    .copied()
                    .find(|candidate| candidate.name() == name)
                    .ok_or(Error::UnknownModel(name))?;
            }
            "--device" => {
                let name = arguments.next().ok_or(Error::Usage)?;
                device = match name.as_str() {
                    "auto" => DevicePreference::Auto,
                    "gpu" => DevicePreference::Gpu,
                    "cpu" => DevicePreference::Cpu,
                    _ => return Err(Error::UnknownDevice(name)),
                };
            }
            "--output" => {
                let directory = PathBuf::from(arguments.next().ok_or(Error::Usage)?);
                output = OutputChoice::Custom(OutputDir::new(directory)?);
            }
            "--replace" => overwrite = OverwritePolicy::Replace,
            _ => return Err(Error::Usage),
        }
    }
    Ok(SeparationRequest::new(
        input, &output, model, device, overwrite,
    ))
}

fn run() -> Result<(), Error> {
    let request = parse(env::args().skip(1))?;
    let store = ModelStore::in_user_cache()?;
    println!("input: {}", request.input().path().display());
    println!(
        "model: {} ({})",
        request.model().name(),
        if store.is_cached(request.model()) {
            "cached"
        } else {
            "will be downloaded"
        }
    );

    let started = Instant::now();
    // Download progress arrives every 64 KiB; print it once per percent.
    let last_percent = Cell::new(u64::MAX);
    let report = |progress: Progress| match progress {
        Progress::FetchingModel(download) => {
            let total = download.total().get().max(1);
            let percent = download.downloaded().get().saturating_mul(100) / total;
            if last_percent.replace(percent) != percent {
                println!("downloading the model: {percent}%");
            }
        }
        Progress::Decoding => println!("decoding the input"),
        Progress::LoadingModel => println!("loading the model"),
        Progress::Separating {
            done,
            total,
            device,
        } => println!(
            "chunk {done}/{total} on {device:?}, {:.1} s elapsed",
            started.elapsed().as_secs_f64()
        ),
        Progress::Finalizing => println!("finishing the files"),
    };

    match job::run(&request, &store, &report, &CancelToken::new())? {
        JobEnd::Completed(output) => {
            for (stem, path) in output.files() {
                println!("{stem:?}: {}", path.display());
            }
            println!(
                "done in {:.1} s, last device {:?}",
                started.elapsed().as_secs_f64(),
                output.device()
            );
        }
        JobEnd::Cancelled => println!("cancelled"),
    }
    Ok(())
}

fn main() -> ExitCode {
    // Without a logger the core's warnings, such as a fall back from the GPU
    // to the CPU, would go unseen. A second logger cannot be installed, which
    // is fine: this is the only one.
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
