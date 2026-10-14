//! The models downloaded so far: how much room they take, showing their
//! folder, and deleting them off the interface thread.

use std::{
    io,
    path::Path,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use eframe::egui;
use stem_split_core::{
    domain::ByteCount,
    model::{CacheError, ModelStore, Usage},
};

/// Why the folder of downloaded models could not be shown or emptied.
#[derive(Debug)]
pub(crate) enum DownloadsProblem {
    Open(io::Error),
    Clear(CacheError),
    /// The thread that deletes the files could not be created.
    NotStarted(io::Error),
    /// That thread ended without reporting, which means a library it uses
    /// panicked.
    Crashed,
}

#[derive(Debug)]
enum State {
    Idle {
        /// As of the last look; `None` if the folder could not be read.
        usage: Option<Usage>,
        /// What went wrong with the last thing the user asked for.
        problem: Option<DownloadsProblem>,
    },
    /// The files are being deleted on another thread.
    Clearing(Receiver<Result<ByteCount, CacheError>>),
}

/// What [`Downloads::poll`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Polled {
    Quiet,
    /// The deletion has just ended, successfully or not. Whatever was known
    /// about which models are on disk is out of date.
    ClearingEnded,
}

/// The model store as the main form shows it.
#[derive(Debug)]
pub(crate) struct Downloads {
    store: ModelStore,
    state: State,
}

impl Downloads {
    pub(crate) fn new(store: ModelStore) -> Self {
        let usage = measure(&store);
        Self {
            store,
            state: State::Idle {
                usage,
                problem: None,
            },
        }
    }

    pub(crate) const fn store(&self) -> &ModelStore {
        &self.store
    }

    pub(crate) fn directory(&self) -> &Path {
        self.store.directory()
    }

    /// How much room the models take, or `None` while they are being
    /// deleted or if the folder could not be read.
    pub(crate) const fn usage(&self) -> Option<Usage> {
        match &self.state {
            State::Idle { usage, .. } => *usage,
            State::Clearing(_) => None,
        }
    }

    pub(crate) const fn is_clearing(&self) -> bool {
        match self.state {
            State::Idle { .. } => false,
            State::Clearing(_) => true,
        }
    }

    pub(crate) const fn problem(&self) -> Option<&DownloadsProblem> {
        match &self.state {
            State::Idle { problem, .. } => problem.as_ref(),
            State::Clearing(_) => None,
        }
    }

    /// Looks at the folder again, for instance after a job has downloaded a
    /// model into it.
    pub(crate) fn refresh(&mut self) {
        match &mut self.state {
            State::Idle { usage, .. } => *usage = measure(&self.store),
            // The deletion measures the folder when it ends.
            State::Clearing(_) => {}
        }
    }

    /// Shows the folder in the file manager. A folder that does not exist
    /// yet has nothing to show; the form offers no way to ask for it.
    pub(crate) fn open_folder(&mut self) {
        match &mut self.state {
            State::Idle { problem, .. } => {
                *problem = open::that_detached(self.store.directory())
                    .err()
                    .map(DownloadsProblem::Open);
            }
            State::Clearing(_) => {}
        }
    }

    /// Starts deleting every downloaded file. `repaint` is asked to redraw
    /// the window when it is done, since an idle window does not redraw.
    pub(crate) fn clear(&mut self, repaint: &egui::Context) {
        match self.state {
            State::Idle { .. } => {}
            State::Clearing(_) => return,
        }
        let (sender, result) = mpsc::channel();
        let store = self.store.clone();
        let repaint = repaint.clone();
        let spawned = thread::Builder::new()
            .name("clearing models".to_owned())
            .spawn(move || {
                // If the window is gone, there is nobody left to tell.
                match sender.send(store.clear()) {
                    Ok(()) | Err(_) => {}
                }
                repaint.request_repaint();
            });
        self.state = match spawned {
            Ok(_) => State::Clearing(result),
            Err(error) => State::Idle {
                usage: measure(&self.store),
                problem: Some(DownloadsProblem::NotStarted(error)),
            },
        };
    }

    /// Takes in the end of a deletion, if it has come, without waiting.
    pub(crate) fn poll(&mut self) -> Polled {
        let State::Clearing(result) = &self.state else {
            return Polled::Quiet;
        };
        let problem = match result.try_recv() {
            Err(TryRecvError::Empty) => return Polled::Quiet,
            Ok(Ok(freed)) => {
                log::info!("deleted {} bytes of downloaded models", freed.get());
                None
            }
            Ok(Err(error)) => Some(DownloadsProblem::Clear(error)),
            Err(TryRecvError::Disconnected) => Some(DownloadsProblem::Crashed),
        };
        self.state = State::Idle {
            usage: measure(&self.store),
            problem,
        };
        Polled::ClearingEnded
    }
}

/// How much room the store takes, or `None` if that cannot be found out. It
/// is only shown, so a failure is logged and not reported.
fn measure(store: &ModelStore) -> Option<Usage> {
    match store.usage() {
        Ok(usage) => Some(usage),
        Err(error) => {
            log::warn!("the downloaded models cannot be measured: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs, time::Duration};

    use super::*;

    const PATIENCE: Duration = Duration::from_secs(10);
    const NAP: Duration = Duration::from_millis(10);

    /// Polls until the deletion ends, or gives up after [`PATIENCE`].
    fn wait_for_clearing(downloads: &mut Downloads) -> Result<(), Box<dyn Error>> {
        let mut waited = Duration::ZERO;
        while downloads.poll() == Polled::Quiet {
            if waited > PATIENCE {
                return Err("the deletion did not end".into());
            }
            thread::sleep(NAP);
            waited += NAP;
        }
        Ok(())
    }

    #[test]
    fn clearing_empties_the_folder_and_measures_it_again() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let store = ModelStore::new(directory.path());
        fs::write(
            directory.path().join("model-0123456789abcdef.onnx"),
            [0_u8; 1_000],
        )?;
        let mut downloads = Downloads::new(store);
        assert_eq!(downloads.usage(), Some(Usage::Files(ByteCount::new(1_000))));

        downloads.clear(&egui::Context::default());
        assert!(downloads.is_clearing());
        assert_eq!(downloads.usage(), None);
        wait_for_clearing(&mut downloads)?;

        assert!(!downloads.is_clearing());
        assert!(downloads.problem().is_none());
        assert_eq!(downloads.usage(), Some(Usage::Files(ByteCount::ZERO)));
        Ok(())
    }

    #[test]
    fn folder_that_cannot_be_read_is_reported_after_clearing() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("models");
        fs::write(&path, b"not a folder")?;
        let mut downloads = Downloads::new(ModelStore::new(&path));
        assert_eq!(downloads.usage(), None);

        downloads.clear(&egui::Context::default());
        wait_for_clearing(&mut downloads)?;

        assert!(matches!(
            downloads.problem(),
            Some(DownloadsProblem::Clear(CacheError::Read { .. }))
        ));
        Ok(())
    }

    #[test]
    fn download_made_by_a_job_shows_up_after_a_refresh() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let models = directory.path().join("models");
        let mut downloads = Downloads::new(ModelStore::new(&models));
        assert_eq!(downloads.usage(), Some(Usage::NoDirectory));

        fs::create_dir(&models)?;
        fs::write(models.join("model-0123456789abcdef.onnx"), [0_u8; 10])?;
        downloads.refresh();

        assert_eq!(downloads.usage(), Some(Usage::Files(ByteCount::new(10))));
        Ok(())
    }
}
