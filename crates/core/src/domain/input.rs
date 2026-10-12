use std::{
    ffi::{OsStr, OsString},
    fs, io,
    path::{self, Path, PathBuf},
};

use super::audio_format::AudioFormat;

/// An audio file chosen for separation.
///
/// Constructing one proves that, at that moment, the path named an existing
/// regular file with a supported extension. The file can still disappear or
/// turn out to be corrupt later; decoding reports that separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputFile {
    path: PathBuf,
    directory: PathBuf,
    base_name: OsString,
    format: AudioFormat,
}

/// Why a path cannot be used as an [`InputFile`].
#[derive(Debug, thiserror::Error)]
pub enum InputFileError {
    #[error("cannot access `{}`: {source}", path.display())]
    Inaccessible {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("`{}` is not a file", path.display())]
    NotAFile { path: PathBuf },
    #[error("`{}` does not have a supported audio file extension", path.display())]
    UnsupportedFormat { path: PathBuf },
}

impl InputFile {
    /// Checks `path` and resolves it against the current directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the path cannot be accessed, is not a regular
    /// file, or its extension is not one of [`AudioFormat`].
    pub fn new(path: impl AsRef<Path>) -> Result<Self, InputFileError> {
        let path = path.as_ref();
        let inaccessible = |source| InputFileError::Inaccessible {
            path: path.to_path_buf(),
            source,
        };
        let path = path::absolute(path).map_err(inaccessible)?;
        let metadata = fs::metadata(&path).map_err(inaccessible)?;

        // An absolute path to a regular file always has both parts; a path
        // without them cannot name a file.
        let (true, Some(directory), Some(base_name)) =
            (metadata.is_file(), path.parent(), path.file_stem())
        else {
            return Err(InputFileError::NotAFile { path });
        };
        let Some(format) = path.extension().and_then(AudioFormat::from_extension) else {
            return Err(InputFileError::UnsupportedFormat { path });
        };

        Ok(Self {
            directory: directory.to_path_buf(),
            base_name: base_name.to_os_string(),
            format,
            path,
        })
    }

    /// Absolute path of the file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The directory containing the file.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// File name without the extension, the base for output file names.
    #[must_use]
    pub fn base_name(&self) -> &OsStr {
        &self.base_name
    }

    #[must_use]
    pub const fn format(&self) -> AudioFormat {
        self.format
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn existing_audio_file_is_accepted() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("My Song.FLAC");
        fs::write(&path, b"not really audio")?;

        let input = InputFile::new(&path)?;

        assert_eq!(input.path(), path);
        assert_eq!(input.directory(), directory.path());
        assert_eq!(input.base_name(), "My Song");
        assert_eq!(input.format(), AudioFormat::Flac);
        Ok(())
    }

    #[test]
    fn base_name_keeps_inner_dots() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("01. intro.v2.mp3");
        fs::write(&path, b"")?;

        assert_eq!(InputFile::new(&path)?.base_name(), "01. intro.v2");
        Ok(())
    }

    #[test]
    fn stored_path_is_absolute() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("song.wav");
        fs::write(&path, b"")?;

        assert!(InputFile::new(&path)?.path().is_absolute());
        Ok(())
    }

    #[test]
    fn missing_file_is_rejected() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("missing.mp3");

        assert!(matches!(
            InputFile::new(&path),
            Err(InputFileError::Inaccessible { path: reported, .. }) if reported == path
        ));
        Ok(())
    }

    #[test]
    fn directory_is_rejected() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("album.mp3");
        fs::create_dir(&path)?;

        assert!(matches!(
            InputFile::new(&path),
            Err(InputFileError::NotAFile { path: reported }) if reported == path
        ));
        Ok(())
    }

    #[test]
    fn unsupported_extension_is_rejected() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        for name in ["notes.txt", "no_extension"] {
            let path = directory.path().join(name);
            fs::write(&path, b"")?;

            assert!(matches!(
                InputFile::new(&path),
                Err(InputFileError::UnsupportedFormat { path: reported }) if reported == path
            ));
        }
        Ok(())
    }
}
