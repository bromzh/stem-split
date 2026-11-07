//! Raw frames set aside on disk for a later stage of a job.

use std::{
    fs::{self, File},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
};

use super::WriteError;
use crate::FrameSink;

/// Bytes of one stereo frame: two little-endian `f32`.
const FRAME_BYTES: usize = 8;

/// A file of raw stereo frames, written in track order and read back whole.
///
/// It is not a result: the file is deleted when the value is dropped, so a
/// job that fails or is cancelled leaves nothing behind.
#[derive(Debug)]
pub struct Spill {
    path: PathBuf,
    file: BufWriter<File>,
}

impl Spill {
    /// Creates the file at `path`, replacing one left by a job that was
    /// killed before it could clean up.
    ///
    /// # Errors
    ///
    /// Returns [`WriteError::Create`] if the file cannot be created.
    pub fn create(path: PathBuf) -> Result<Self, WriteError> {
        match File::create(&path) {
            Ok(file) => Ok(Self {
                path,
                file: BufWriter::new(file),
            }),
            Err(source) => Err(WriteError::Create { path, source }),
        }
    }

    /// Every frame written so far.
    ///
    /// # Errors
    ///
    /// Returns [`WriteError::Write`] if the file cannot be completed or read.
    pub fn read(&mut self) -> Result<Vec<[f32; 2]>, WriteError> {
        let failed = |path: &Path| {
            let path = path.to_path_buf();
            move |source: io::Error| WriteError::Write {
                path,
                source: hound::Error::IoError(source),
            }
        };
        self.file.flush().map_err(failed(&self.path))?;
        let bytes = fs::read(&self.path).map_err(failed(&self.path))?;
        let (frames, _) = bytes.as_chunks::<FRAME_BYTES>();
        Ok(frames
            .iter()
            .map(|&[l0, l1, l2, l3, r0, r1, r2, r3]| {
                [
                    f32::from_le_bytes([l0, l1, l2, l3]),
                    f32::from_le_bytes([r0, r1, r2, r3]),
                ]
            })
            .collect())
    }
}

impl FrameSink for Spill {
    fn write(&mut self, frames: &[[f32; 2]]) -> Result<(), WriteError> {
        for &[left, right] in frames {
            let mut bytes = [0_u8; FRAME_BYTES];
            let (left_bytes, right_bytes) = bytes.split_at_mut(FRAME_BYTES / 2);
            left_bytes.copy_from_slice(&left.to_le_bytes());
            right_bytes.copy_from_slice(&right.to_le_bytes());
            self.file
                .write_all(&bytes)
                .map_err(|source| WriteError::Write {
                    path: self.path.clone(),
                    source: hound::Error::IoError(source),
                })?;
        }
        Ok(())
    }
}

impl Drop for Spill {
    fn drop(&mut self) {
        // Nothing can be done about a file that cannot be removed; the next
        // job with the same name replaces it.
        match fs::remove_file(&self.path) {
            Ok(()) | Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn frames_come_back_as_written_and_the_file_goes() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("song.vocals.wav.raw");
        let frames = [[0.25, -1.5], [f32::MAX, f32::MIN_POSITIVE], [0.0, -0.0]];

        let mut spill = Spill::create(path.clone())?;
        spill.write(&frames)?;
        spill.write(&frames)?;
        let read = spill.read()?;

        assert_eq!(read.len(), 6);
        assert_eq!(read.get(3..), Some(frames.as_slice()));
        assert!(path.exists());
        drop(spill);
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn leftover_file_is_replaced() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("song.bass.wav.raw");
        fs::write(&path, b"left by a killed job")?;

        let mut spill = Spill::create(path)?;

        assert!(spill.read()?.is_empty());
        Ok(())
    }
}
