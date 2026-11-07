use std::{
    fs, io,
    path::{self, Path, PathBuf},
};

use super::input::InputFile;

/// A directory that receives the separated stems.
///
/// Constructing one proves that, at that moment, the path named an existing
/// directory. Whether it is writable is only known once a file is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDir(PathBuf);

/// Why a path cannot be used as an [`OutputDir`].
#[derive(Debug, thiserror::Error)]
pub enum OutputDirError {
    #[error("cannot access `{}`: {source}", path.display())]
    Inaccessible {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("`{}` is not a directory", path.display())]
    NotADirectory { path: PathBuf },
}

impl OutputDir {
    /// Checks `path` and resolves it against the current directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the path cannot be accessed or is not a directory.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, OutputDirError> {
        let path = path.as_ref();
        let inaccessible = |source| OutputDirError::Inaccessible {
            path: path.to_path_buf(),
            source,
        };
        let path = path::absolute(path).map_err(inaccessible)?;
        let metadata = fs::metadata(&path).map_err(inaccessible)?;
        if !metadata.is_dir() {
            return Err(OutputDirError::NotADirectory { path });
        }
        Ok(Self(path))
    }

    /// The directory the input file is in. It needs no check: it holds a file
    /// that was already found to exist.
    #[must_use]
    pub fn beside(input: &InputFile) -> Self {
        Self(input.directory().to_path_buf())
    }

    /// Absolute path of the directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Where the user wants the stems written.
///
/// "Next to the input" is kept as a choice of its own, not resolved to a
/// directory, so that it follows the input file when the user picks another.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OutputChoice {
    #[default]
    BesideInput,
    Custom(OutputDir),
}

impl OutputChoice {
    /// The directory this choice means for `input`.
    #[must_use]
    pub fn resolve(&self, input: &InputFile) -> OutputDir {
        match self {
            Self::BesideInput => OutputDir::beside(input),
            Self::Custom(directory) => directory.clone(),
        }
    }
}

/// What to do when an output file already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverwritePolicy {
    /// Fail without touching the existing file.
    #[default]
    Refuse,
    /// Replace the existing file. Requires the user's explicit consent.
    Replace,
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    fn input_in(directory: &Path) -> Result<InputFile, Box<dyn Error>> {
        let path = directory.join("song.mp3");
        fs::write(&path, b"")?;
        Ok(InputFile::new(path)?)
    }

    #[test]
    fn existing_directory_is_accepted() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;

        let output = OutputDir::new(directory.path())?;

        assert_eq!(output.path(), directory.path());
        assert!(output.path().is_absolute());
        Ok(())
    }

    #[test]
    fn missing_directory_is_rejected() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("missing");

        assert!(matches!(
            OutputDir::new(&path),
            Err(OutputDirError::Inaccessible { path: reported, .. }) if reported == path
        ));
        Ok(())
    }

    #[test]
    fn file_is_rejected() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("file.txt");
        fs::write(&path, b"")?;

        assert!(matches!(
            OutputDir::new(&path),
            Err(OutputDirError::NotADirectory { path: reported }) if reported == path
        ));
        Ok(())
    }

    #[test]
    fn beside_input_is_the_default_and_follows_the_input() -> Result<(), Box<dyn Error>> {
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        let choice = OutputChoice::default();

        assert_eq!(choice, OutputChoice::BesideInput);
        assert_eq!(
            choice.resolve(&input_in(first.path())?).path(),
            first.path()
        );
        assert_eq!(
            choice.resolve(&input_in(second.path())?).path(),
            second.path()
        );
        Ok(())
    }

    #[test]
    fn custom_directory_ignores_the_input_location() -> Result<(), Box<dyn Error>> {
        let music = tempfile::tempdir()?;
        let stems = tempfile::tempdir()?;
        let choice = OutputChoice::Custom(OutputDir::new(stems.path())?);

        assert_eq!(
            choice.resolve(&input_in(music.path())?).path(),
            stems.path()
        );
        Ok(())
    }

    #[test]
    fn existing_files_are_kept_by_default() {
        assert_eq!(OverwritePolicy::default(), OverwritePolicy::Refuse);
    }
}
