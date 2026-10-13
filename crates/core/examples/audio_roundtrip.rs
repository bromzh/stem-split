//! Decodes a file, converts it to the model sample rate and optionally writes
//! the result as WAV. A manual check of the audio stages on real-world files:
//!
//! ```sh
//! cargo run --release -p stem-split-core --example audio_roundtrip -- song.mp3 out.wav
//! ```

use std::{env, path::PathBuf, process::ExitCode, time::Instant};

use stem_split_core::{
    audio::{self, DecodeError, ResampleError, WavWriter, WriteError},
    domain::{FrameCount, InputFile, InputFileError, OverwritePolicy, SampleRate},
};

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("usage: audio_roundtrip <input> [output.wav]")]
    Usage,
    #[error(transparent)]
    Input(#[from] InputFileError),
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Resample(#[from] ResampleError),
    #[error(transparent)]
    Write(#[from] WriteError),
}

/// Duration of `frames` at `rate`, in milliseconds.
fn milliseconds(frames: FrameCount, rate: SampleRate) -> u64 {
    let frames = u64::try_from(frames.get()).unwrap_or(u64::MAX);
    frames.saturating_mul(1000) / u64::from(rate.hertz().get())
}

fn run() -> Result<(), Error> {
    let mut arguments = env::args_os().skip(1);
    let input = arguments.next().map(PathBuf::from).ok_or(Error::Usage)?;
    let output = arguments.next().map(PathBuf::from);

    let input = InputFile::new(input)?;
    println!("input: {} ({:?})", input.path().display(), input.format());

    let started = Instant::now();
    let decoded = audio::decode(&input)?;
    println!(
        "decoded: {} Hz, {} frames, {} ms of audio, in {} ms",
        decoded.rate().hertz(),
        decoded.frames().len().get(),
        milliseconds(decoded.frames().len(), decoded.rate()),
        started.elapsed().as_millis()
    );
    let peak = decoded
        .frames()
        .as_slice()
        .as_flattened()
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    println!("peak level: {peak:.4}");

    let started = Instant::now();
    let resampled = audio::resample(decoded)?;
    println!(
        "at model rate: {} frames, {} ms of audio, in {} ms",
        resampled.frames().len().get(),
        milliseconds(resampled.frames().len(), SampleRate::MODEL),
        started.elapsed().as_millis()
    );

    if let Some(output) = output {
        let started = Instant::now();
        let mut writer = WavWriter::create(&output, SampleRate::MODEL, OverwritePolicy::Refuse)?;
        writer.write(resampled.frames().as_slice())?;
        writer.finish()?;
        println!(
            "written: {} in {} ms",
            output.display(),
            started.elapsed().as_millis()
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
