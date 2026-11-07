//! Background tool management for the settings page.

use eframe::egui;
use std::{
    io,
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use stem_split_core::{
    CancelToken,
    model::{ModelStore, Usage},
};
use stem_split_media::{Tool, tools::Action};

#[derive(Debug, thiserror::Error)]
pub(crate) enum Problem {
    #[error("cannot start tool worker: {0}")]
    Start(#[source] io::Error),
    #[error(transparent)]
    Media(#[from] stem_split_media::DownloadError),
    #[error("cannot open cache folder: {0}")]
    Folder(#[source] io::Error),
    #[error("tool worker stopped unexpectedly")]
    Crashed,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Operation {
    Scan,
    Manage(Tool, Action),
    OpenFolder,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Installed {
    downloader: bool,
    ffmpeg: bool,
}

impl Installed {
    pub(crate) const fn contains(self, tool: Tool) -> bool {
        match tool {
            Tool::Downloader => self.downloader,
            Tool::Ffmpeg => self.ffmpeg,
        }
    }
}

#[derive(Debug)]
struct Work {
    result: Receiver<(Installed, Option<Usage>, Result<(), Problem>)>,
    thread: JoinHandle<()>,
    cancel: CancelToken,
}

#[derive(Debug, Default)]
pub(crate) struct Tools {
    installed: Option<Installed>,
    usage: Option<Usage>,
    work: Option<Work>,
    problem: Option<Problem>,
}

impl Tools {
    pub(crate) const fn usage(&self) -> Option<Usage> {
        self.usage
    }

    pub(crate) const fn busy(&self) -> bool {
        self.work.is_some()
    }
    pub(crate) const fn installed(&self) -> Option<Installed> {
        self.installed
    }
    pub(crate) const fn problem(&self) -> Option<&Problem> {
        self.problem.as_ref()
    }

    pub(crate) fn start(&mut self, cache: PathBuf, operation: Operation, context: &egui::Context) {
        if self.busy() {
            return;
        }
        let (sender, result) = mpsc::channel();
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let repaint = context.clone();
        let spawned = thread::Builder::new()
            .name("media tool management".to_owned())
            .spawn(move || {
                let outcome = match operation {
                    Operation::Manage(tool, action) => {
                        stem_split_media::tools::manage(&cache, tool, action, &token)
                            .map_err(Problem::Media)
                    }
                    Operation::Scan => Ok(()),
                    Operation::OpenFolder => std::fs::create_dir_all(&cache)
                        .and_then(|()| open::that_detached(&cache))
                        .map_err(Problem::Folder),
                };
                let installed = Installed {
                    downloader: stem_split_media::tools::path(&cache, Tool::Downloader).is_file(),
                    ffmpeg: stem_split_media::tools::path(&cache, Tool::Ffmpeg).is_file(),
                };
                let usage = match ModelStore::new(&cache).usage() {
                    Ok(usage) => Some(usage),
                    Err(error) => {
                        log::warn!("cannot measure tool cache: {error}");
                        None
                    }
                };
                match sender.send((installed, usage, outcome)) {
                    Ok(()) | Err(_) => {}
                }
                repaint.request_repaint();
            });
        self.problem = None;
        match spawned {
            Ok(thread) => {
                self.work = Some(Work {
                    result,
                    thread,
                    cancel,
                });
            }
            Err(error) => self.problem = Some(Problem::Start(error)),
        }
    }

    pub(crate) fn poll(&mut self) {
        let Some(work) = &self.work else {
            return;
        };
        if !work.thread.is_finished() {
            return;
        }
        match work.result.try_recv() {
            Ok((installed, usage, outcome)) => {
                self.usage = usage;
                self.installed = Some(installed);
                self.problem = outcome.err();
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                self.problem = Some(Problem::Crashed);
            }
        }
        if let Some(work) = self.work.take()
            && work.thread.join().is_err()
        {
            self.problem = Some(Problem::Crashed);
        }
    }

    pub(crate) fn stop(&mut self, patience: Duration) {
        self.cancel();
        let deadline = Instant::now() + patience;
        while self.busy() && Instant::now() < deadline {
            self.poll();
            if self.busy() {
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    pub(crate) fn cancel(&self) {
        if let Some(work) = &self.work {
            work.cancel.cancel();
        }
    }
}

impl Drop for Tools {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(tools: &mut Tools) -> Result<(), io::Error> {
        tools.stop(Duration::from_secs(10));
        if tools.busy() {
            return Err(io::Error::other("tool worker did not finish"));
        }
        if let Some(problem) = tools.problem() {
            return Err(io::Error::other(problem.to_string()));
        }
        Ok(())
    }

    #[test]
    fn worker_reports_installation_state_and_refreshes_after_removal() -> Result<(), io::Error> {
        let directory = tempfile::tempdir()?;
        let cache = directory.path();
        std::fs::write(
            stem_split_media::tools::path(cache, Tool::Ffmpeg),
            b"binary",
        )?;
        let context = egui::Context::default();
        let mut tools = Tools::default();
        tools.start(cache.to_path_buf(), Operation::Scan, &context);
        wait(&mut tools)?;
        assert!(
            tools
                .installed()
                .is_some_and(|state| state.contains(Tool::Ffmpeg))
        );
        tools.start(
            cache.to_path_buf(),
            Operation::Manage(Tool::Ffmpeg, Action::Remove),
            &context,
        );
        wait(&mut tools)?;
        assert!(
            tools
                .installed()
                .is_some_and(|state| !state.contains(Tool::Ffmpeg))
        );
        assert_eq!(
            tools.usage(),
            Some(Usage::Files(stem_split_core::domain::ByteCount::ZERO))
        );
        Ok(())
    }
}
