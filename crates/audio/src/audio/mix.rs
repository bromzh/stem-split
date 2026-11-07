//! Summing finished stem files into one, for a mix made after the stems
//! were written — for instance from cleaned-up stems.

use std::path::{Path, PathBuf};

use super::{WavWriter, WriteError, decode};
use crate::domain::{
    FrameCount, InputFile, InputFileError, OverwritePolicy, SampleRate, StereoFrames,
};

/// Why files could not be summed into a mix.
#[derive(Debug, thiserror::Error)]
pub enum MixError {
    #[error(transparent)]
    Input(#[from] InputFileError),
    #[error(transparent)]
    Decode(#[from] super::DecodeError),
    #[error("`{}` is not at the model sample rate", path.display())]
    SampleRate { path: PathBuf },
    #[error("`{}` does not have the same length as the first file", path.display())]
    Length { path: PathBuf },
    #[error(transparent)]
    Write(#[from] WriteError),
}

fn read_at_model_rate(path: &Path) -> Result<StereoFrames, MixError> {
    let audio = decode(&InputFile::new(path)?)?;
    if audio.rate() != SampleRate::MODEL {
        return Err(MixError::SampleRate {
            path: path.to_path_buf(),
        });
    }
    let (_, frames) = audio.into_parts();
    Ok(frames)
}

/// Sums `first` and `rest` sample by sample and writes the result to
/// `output`, which appears under its final name only when done.
///
/// The files are summed one at a time, so at most two of them are in memory.
/// Every file must be at the model sample rate and have the same length as
/// the first one — both hold for the stem files of one job.
///
/// # Errors
///
/// Returns an error if a file cannot be read as audio, has the wrong sample
/// rate or length, or if the output cannot be written.
pub fn mix_files(
    first: &Path,
    rest: &[PathBuf],
    output: &Path,
    overwrite: OverwritePolicy,
) -> Result<(), MixError> {
    let mut sum = read_at_model_rate(first)?.into_inner();
    for path in rest {
        let frames = read_at_model_rate(path)?;
        if frames.len() != FrameCount::new(sum.len()) {
            return Err(MixError::Length { path: path.clone() });
        }
        for (held, &[left, right]) in sum.iter_mut().zip(frames.as_slice()) {
            let [held_left, held_right] = held;
            *held_left += left;
            *held_right += right;
        }
    }

    let mut writer = WavWriter::create(output, SampleRate::MODEL, overwrite)?;
    writer.write(&sum)?;
    writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs};

    use super::*;

    fn write(path: &Path, frames: &[[f32; 2]]) -> Result<(), Box<dyn Error>> {
        let mut writer = WavWriter::create(path, SampleRate::MODEL, OverwritePolicy::Refuse)?;
        writer.write(frames)?;
        writer.finish()?;
        Ok(())
    }

    #[test]
    fn files_are_summed_sample_by_sample() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("song.drums.wav");
        let second = directory.path().join("song.vocals.wav");
        let output = directory.path().join("song.mix.wav");
        write(&first, &[[0.25, -0.5], [1.5, 0.0]])?;
        write(&second, &[[0.5, 0.25], [-0.5, 2.0]])?;

        mix_files(&first, &[second], &output, OverwritePolicy::Refuse)?;

        let audio = decode(&InputFile::new(&output)?)?;
        assert_eq!(audio.frames().as_slice(), [[0.75, -0.25], [1.0, 2.0]]);
        Ok(())
    }

    #[test]
    fn a_shorter_file_is_an_error_and_nothing_is_written() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("song.drums.wav");
        let second = directory.path().join("song.vocals.wav");
        let output = directory.path().join("song.mix.wav");
        write(&first, &[[0.25, 0.25]; 10])?;
        write(&second, &[[0.25, 0.25]; 9])?;

        let result = mix_files(&first, &[second], &output, OverwritePolicy::Refuse);

        assert!(matches!(result, Err(MixError::Length { .. })));
        assert!(!output.exists());
        Ok(())
    }

    #[test]
    fn a_missing_file_is_an_error() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let missing = directory.path().join("gone.wav");
        let output = directory.path().join("song.mix.wav");

        let result = mix_files(&missing, &[], &output, OverwritePolicy::Refuse);

        assert!(matches!(result, Err(MixError::Input(_))));
        assert!(!output.exists());
        Ok(())
    }

    #[test]
    fn an_existing_mix_is_refused_unless_replacing() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("song.drums.wav");
        let output = directory.path().join("song.mix.wav");
        write(&first, &[[0.5, 0.5]])?;
        fs::write(&output, b"previous")?;

        let refused = mix_files(&first, &[], &output, OverwritePolicy::Refuse);
        assert!(matches!(
            refused,
            Err(MixError::Write(WriteError::AlreadyExists { .. }))
        ));
        assert_eq!(fs::read(&output)?, b"previous");

        mix_files(&first, &[], &output, OverwritePolicy::Replace)?;
        let audio = decode(&InputFile::new(&output)?)?;
        assert_eq!(audio.frames().as_slice(), [[0.5, 0.5]]);
        Ok(())
    }
}
