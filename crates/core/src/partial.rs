//! Files that appear under their final name only once they are complete.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use crate::domain::OverwritePolicy;

/// Appended to the target file name to get the name data is written under.
const PART_SUFFIX: &str = ".part";

#[derive(Debug, thiserror::Error)]
pub(crate) enum PartialFileError {
    #[error("`{}` already exists", path.display())]
    TargetExists { path: PathBuf },
    #[error("`{}` does not name a file", path.display())]
    InvalidTarget { path: PathBuf },
    #[error("cannot create `{}`: {source}", path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot move the finished file to `{}`: {source}", path.display())]
    Publish {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Whether an empty file was created at the target path to claim its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Claim {
    Placeholder,
    Unclaimed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Pending,
    Published,
}

/// A file being written as `<target>.part` next to its target.
///
/// [`PartialFile::publish`] moves it into place. Dropping the guard without
/// publishing removes everything it created, so a failed or cancelled write
/// leaves no half-written file behind and, under
/// [`OverwritePolicy::Replace`], keeps the previous file intact.
#[derive(Debug)]
pub(crate) struct PartialFile {
    target: PathBuf,
    part: PathBuf,
    claim: Claim,
    state: State,
}

impl PartialFile {
    /// Starts a file that will end up at `target`, returning the guard and
    /// the handle to write through.
    ///
    /// Under [`OverwritePolicy::Refuse`] the target name is claimed right
    /// away with an empty placeholder, created atomically: of two writers
    /// racing for one name, exactly one gets it.
    pub(crate) fn create(
        target: &Path,
        policy: OverwritePolicy,
    ) -> Result<(Self, File), PartialFileError> {
        let Some(name) = target.file_name() else {
            return Err(PartialFileError::InvalidTarget {
                path: target.to_path_buf(),
            });
        };
        let mut part_name = name.to_os_string();
        part_name.push(PART_SUFFIX);
        let part = target.with_file_name(part_name);

        let claim = match policy {
            OverwritePolicy::Refuse => {
                match OpenOptions::new().write(true).create_new(true).open(target) {
                    Ok(placeholder) => {
                        drop(placeholder);
                        Claim::Placeholder
                    }
                    Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                        return Err(PartialFileError::TargetExists {
                            path: target.to_path_buf(),
                        });
                    }
                    Err(source) => {
                        return Err(PartialFileError::Create {
                            path: target.to_path_buf(),
                            source,
                        });
                    }
                }
            }
            OverwritePolicy::Replace => Claim::Unclaimed,
        };

        // From here the guard owns the cleanup, including on the error path below.
        let guard = Self {
            target: target.to_path_buf(),
            part,
            claim,
            state: State::Pending,
        };
        let file = File::create(&guard.part).map_err(|source| PartialFileError::Create {
            path: guard.part.clone(),
            source,
        })?;
        Ok((guard, file))
    }

    pub(crate) fn target(&self) -> &Path {
        &self.target
    }

    /// Moves the finished file to its target, replacing a placeholder or,
    /// under [`OverwritePolicy::Replace`], the previous file. The handle
    /// returned by [`PartialFile::create`] must be closed first.
    pub(crate) fn publish(mut self) -> Result<(), PartialFileError> {
        fs::rename(&self.part, &self.target).map_err(|source| PartialFileError::Publish {
            path: self.target.clone(),
            source,
        })?;
        self.state = State::Published;
        Ok(())
    }
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        match self.state {
            State::Published => {}
            State::Pending => {
                remove_quietly(&self.part);
                match self.claim {
                    Claim::Placeholder => remove_quietly(&self.target),
                    Claim::Unclaimed => {}
                }
            }
        }
    }
}

/// Removes a file from `Drop`, where a failure cannot be reported. The worst
/// outcome is a leftover file, which the next write either replaces or
/// reports as already existing.
fn remove_quietly(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) | Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, io::Write};

    use super::*;

    fn part_of(target: &Path) -> PathBuf {
        let mut name = target.as_os_str().to_os_string();
        name.push(PART_SUFFIX);
        PathBuf::from(name)
    }

    #[test]
    fn published_file_replaces_the_placeholder() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("stem.wav");

        let (partial, mut file) = PartialFile::create(&target, OverwritePolicy::Refuse)?;
        assert_eq!(partial.target(), target);
        file.write_all(b"audio")?;
        // While writing, the target name is held by an empty placeholder.
        assert_eq!(fs::read(&target)?, b"");
        assert!(part_of(&target).exists());
        drop(file);
        partial.publish()?;

        assert_eq!(fs::read(&target)?, b"audio");
        assert!(!part_of(&target).exists());
        Ok(())
    }

    #[test]
    fn dropping_without_publishing_leaves_nothing() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("stem.wav");

        let (partial, mut file) = PartialFile::create(&target, OverwritePolicy::Refuse)?;
        file.write_all(b"half")?;
        drop(file);
        drop(partial);

        assert!(!target.exists());
        assert!(!part_of(&target).exists());
        Ok(())
    }

    #[test]
    fn existing_target_is_refused_and_left_untouched() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("stem.wav");
        fs::write(&target, b"previous")?;

        let result = PartialFile::create(&target, OverwritePolicy::Refuse);

        assert!(matches!(
            result,
            Err(PartialFileError::TargetExists { path }) if path == target
        ));
        assert_eq!(fs::read(&target)?, b"previous");
        assert!(!part_of(&target).exists());
        Ok(())
    }

    #[test]
    fn replace_keeps_the_previous_file_until_published() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("stem.wav");
        fs::write(&target, b"previous")?;

        let (partial, mut file) = PartialFile::create(&target, OverwritePolicy::Replace)?;
        file.write_all(b"new")?;
        drop(file);
        assert_eq!(fs::read(&target)?, b"previous");
        partial.publish()?;

        assert_eq!(fs::read(&target)?, b"new");
        assert!(!part_of(&target).exists());
        Ok(())
    }

    #[test]
    fn abandoned_replace_keeps_the_previous_file() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("stem.wav");
        fs::write(&target, b"previous")?;

        let (partial, file) = PartialFile::create(&target, OverwritePolicy::Replace)?;
        drop(file);
        drop(partial);

        assert_eq!(fs::read(&target)?, b"previous");
        assert!(!part_of(&target).exists());
        Ok(())
    }

    #[test]
    fn replace_creates_a_missing_target() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("stem.wav");

        let (partial, mut file) = PartialFile::create(&target, OverwritePolicy::Replace)?;
        file.write_all(b"new")?;
        drop(file);
        // Nothing is claimed under `Replace`, so the target does not exist yet.
        assert!(!target.exists());
        partial.publish()?;

        assert_eq!(fs::read(&target)?, b"new");
        Ok(())
    }

    #[test]
    fn stale_part_file_is_overwritten() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("stem.wav");
        fs::write(part_of(&target), b"left over from a crash")?;

        let (partial, mut file) = PartialFile::create(&target, OverwritePolicy::Refuse)?;
        file.write_all(b"new")?;
        drop(file);
        partial.publish()?;

        assert_eq!(fs::read(&target)?, b"new");
        Ok(())
    }

    #[test]
    fn missing_directory_is_reported_and_leaves_nothing() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("missing").join("stem.wav");

        for policy in [OverwritePolicy::Refuse, OverwritePolicy::Replace] {
            assert!(matches!(
                PartialFile::create(&target, policy),
                Err(PartialFileError::Create { .. })
            ));
        }
        assert!(!directory.path().join("missing").exists());
        Ok(())
    }

    #[test]
    fn path_without_a_file_name_is_rejected() {
        let target = Path::new("..");
        assert!(matches!(
            PartialFile::create(target, OverwritePolicy::Replace),
            Err(PartialFileError::InvalidTarget { path }) if path == target
        ));
    }
}
