use std::{
    fmt,
    fs::File,
    io::{self, BufWriter},
    path::{Path, PathBuf},
};

use hound::{SampleFormat, WavSpec};

use crate::{
    domain::{OverwritePolicy, SampleRate},
    partial::{PartialFile, PartialFileError},
};

const CHANNELS: u16 = 2;
const BITS_PER_SAMPLE: u16 = 32;
const BYTES_PER_FRAME: u64 = 8;

/// Most audio data a file may hold. WAV stores sizes in 32 bits; the margin
/// leaves room for the header. `hound` does not check this itself and would
/// overflow its counter.
const MAX_DATA_BYTES: u64 = 0xFFFF_F000;

/// Why a stem could not be written.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("`{}` already exists", path.display())]
    AlreadyExists { path: PathBuf },
    #[error("cannot create `{}`: {source}", path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write `{}`: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: hound::Error,
    },
    #[error("cannot move the finished file to `{}`: {source}", path.display())]
    Publish {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("`{}` would exceed the 4 GiB limit of the WAV format", path.display())]
    TooLong { path: PathBuf },
}

impl From<PartialFileError> for WriteError {
    fn from(error: PartialFileError) -> Self {
        match error {
            PartialFileError::TargetExists { path } => Self::AlreadyExists { path },
            PartialFileError::InvalidTarget { path } => Self::Create {
                source: io::Error::new(io::ErrorKind::InvalidInput, "path does not name a file"),
                path,
            },
            PartialFileError::Create { path, source } => Self::Create { path, source },
            PartialFileError::Publish { path, source } => Self::Publish { path, source },
        }
    }
}

/// Writes stereo audio to a 32-bit float WAV file, a piece at a time.
///
/// The data goes to a temporary file next to the target. Only
/// [`WavWriter::finish`] puts it in place; a writer that is dropped instead,
/// because of an error or a cancelled job, removes what it wrote.
pub struct WavWriter {
    // Declared before `partial` so that the file is closed before the guard
    // decides what to do with it.
    writer: hound::WavWriter<BufWriter<File>>,
    partial: PartialFile,
    data_bytes: u64,
}

// `hound::WavWriter` has no `Debug` implementation to derive from.
impl fmt::Debug for WavWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WavWriter")
            .field("partial", &self.partial)
            .field("data_bytes", &self.data_bytes)
            .finish_non_exhaustive()
    }
}

impl WavWriter {
    /// Starts a file that will end up at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`WriteError::AlreadyExists`] if the file exists and `policy`
    /// is [`OverwritePolicy::Refuse`], or [`WriteError::Create`] if the file
    /// cannot be created.
    pub fn create(
        path: &Path,
        rate: SampleRate,
        policy: OverwritePolicy,
    ) -> Result<Self, WriteError> {
        let (partial, file) = PartialFile::create(path, policy)?;
        let spec = WavSpec {
            channels: CHANNELS,
            sample_rate: rate.hertz().get(),
            bits_per_sample: BITS_PER_SAMPLE,
            sample_format: SampleFormat::Float,
        };
        let writer = hound::WavWriter::new(BufWriter::new(file), spec).map_err(|source| {
            WriteError::Write {
                path: path.to_path_buf(),
                source,
            }
        })?;
        Ok(Self {
            writer,
            partial,
            data_bytes: 0,
        })
    }

    /// Appends frames to the file.
    ///
    /// # Errors
    ///
    /// Returns [`WriteError::TooLong`] if the file would outgrow the WAV
    /// format, or [`WriteError::Write`] if the data cannot be written.
    pub fn write(&mut self, frames: &[[f32; 2]]) -> Result<(), WriteError> {
        let path = || self.partial.target().to_path_buf();
        let data_bytes = u64::try_from(frames.len())
            .ok()
            .and_then(|count| count.checked_mul(BYTES_PER_FRAME))
            .and_then(|added| self.data_bytes.checked_add(added))
            .filter(|&total| total <= MAX_DATA_BYTES)
            .ok_or_else(|| WriteError::TooLong { path: path() })?;

        for &sample in frames.as_flattened() {
            self.writer
                .write_sample(sample)
                .map_err(|source| WriteError::Write {
                    path: path(),
                    source,
                })?;
        }
        self.data_bytes = data_bytes;
        Ok(())
    }

    /// Completes the file and moves it to its final path.
    ///
    /// # Errors
    ///
    /// Returns an error if the remaining data cannot be written or the file
    /// cannot be moved into place. Nothing is left on disk in that case.
    pub fn finish(self) -> Result<(), WriteError> {
        let Self {
            writer, partial, ..
        } = self;
        writer.finalize().map_err(|source| WriteError::Write {
            path: partial.target().to_path_buf(),
            source,
        })?;
        partial.publish()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs};

    use super::*;
    use crate::{audio::decode, domain::InputFile};

    fn part_of(target: &Path) -> PathBuf {
        let mut name = target.as_os_str().to_os_string();
        name.push(".part");
        PathBuf::from(name)
    }

    #[test]
    fn written_audio_reads_back_exactly() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("song.vocals.wav");
        let first = [[0.0_f32, 1.0], [-1.0, 0.5]];
        let second = [[0.123_456_79_f32, -0.987_654_3], [1.5, -2.5]];

        let mut writer = WavWriter::create(&path, SampleRate::MODEL, OverwritePolicy::Refuse)?;
        writer.write(&first)?;
        writer.write(&second)?;
        writer.finish()?;

        let audio = decode(&InputFile::new(&path)?)?;
        assert_eq!(audio.rate(), SampleRate::MODEL);
        // Float samples are stored as is: values beyond full scale survive too.
        assert_eq!(audio.frames().as_slice(), [first, second].concat());
        assert!(!part_of(&path).exists());
        Ok(())
    }

    #[test]
    fn header_describes_stereo_float_at_the_given_rate() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("stem.wav");
        let rate = SampleRate::from_hertz(48_000).ok_or("rate")?;

        let mut writer = WavWriter::create(&path, rate, OverwritePolicy::Refuse)?;
        writer.write(&[[0.0, 0.0]; 10])?;
        writer.finish()?;

        let reader = hound::WavReader::open(&path)?;
        assert_eq!(
            reader.spec(),
            WavSpec {
                channels: 2,
                sample_rate: 48_000,
                bits_per_sample: 32,
                sample_format: SampleFormat::Float,
            }
        );
        assert_eq!(reader.duration(), 10);
        Ok(())
    }

    #[test]
    fn unfinished_file_disappears() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("stem.wav");

        let mut writer = WavWriter::create(&path, SampleRate::MODEL, OverwritePolicy::Refuse)?;
        writer.write(&[[0.5, 0.5]; 1000])?;
        assert!(part_of(&path).exists());
        drop(writer);

        assert!(!path.exists());
        assert!(!part_of(&path).exists());
        assert_eq!(fs::read_dir(directory.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn existing_file_is_refused_and_kept() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("stem.wav");
        fs::write(&path, b"previous")?;

        let result = WavWriter::create(&path, SampleRate::MODEL, OverwritePolicy::Refuse);

        assert!(matches!(
            result,
            Err(WriteError::AlreadyExists { path: reported }) if reported == path
        ));
        assert_eq!(fs::read(&path)?, b"previous");
        Ok(())
    }

    #[test]
    fn existing_file_is_replaced_only_on_finish() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("stem.wav");
        fs::write(&path, b"previous")?;

        let mut abandoned = WavWriter::create(&path, SampleRate::MODEL, OverwritePolicy::Replace)?;
        abandoned.write(&[[0.1, 0.1]])?;
        drop(abandoned);
        assert_eq!(fs::read(&path)?, b"previous");

        let mut writer = WavWriter::create(&path, SampleRate::MODEL, OverwritePolicy::Replace)?;
        writer.write(&[[0.25, -0.25]])?;
        writer.finish()?;

        let audio = decode(&InputFile::new(&path)?)?;
        assert_eq!(audio.frames().as_slice(), [[0.25, -0.25]]);
        Ok(())
    }

    #[test]
    fn missing_directory_is_reported() {
        let path = Path::new("no-such-directory-for-stem-split").join("stem.wav");

        assert!(matches!(
            WavWriter::create(&path, SampleRate::MODEL, OverwritePolicy::Refuse),
            Err(WriteError::Create { .. })
        ));
    }

    #[test]
    fn size_limit_is_checked_before_writing() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("stem.wav");
        let mut writer = WavWriter::create(&path, SampleRate::MODEL, OverwritePolicy::Refuse)?;

        // Pretend the file is one frame short of full without writing 4 GiB.
        writer.data_bytes = MAX_DATA_BYTES - BYTES_PER_FRAME;
        writer.write(&[[0.0, 0.0]])?;
        assert!(matches!(
            writer.write(&[[0.0, 0.0]]),
            Err(WriteError::TooLong { path: reported }) if reported == path
        ));
        Ok(())
    }
}
