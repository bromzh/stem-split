//! Running a separation job off the interface thread.

use std::{
    io,
    sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use eframe::egui;
use stem_split_core::{
    CancelToken,
    job::{self, JobEnd, JobError, Progress, SeparationOutput, SeparationRequest},
    model::ModelStore,
};

/// Why a job did not produce its files.
#[derive(Debug)]
pub(crate) enum Failure {
    Job(JobError),
    /// The worker thread could not be created.
    NotStarted(io::Error),
    /// The worker thread ended without reporting a result, which means a
    /// library it uses panicked.
    Crashed,
}

/// How a job ended.
#[derive(Debug)]
pub(crate) enum Outcome {
    Completed(SeparationOutput),
    Cancelled,
    Failed(Failure),
}

/// What the worker thread tells the interface thread.
enum Event {
    Progress(Progress),
    Finished(Box<Result<JobEnd, JobError>>),
}

/// What arrived from the worker since the last look.
#[derive(Debug)]
pub(crate) struct Polled {
    /// The most recent progress report, if there was any.
    pub(crate) progress: Option<Progress>,
    pub(crate) outcome: Option<Outcome>,
}

/// A job running on its own thread.
#[derive(Debug)]
pub(crate) struct Worker {
    events: Receiver<Event>,
    cancel: CancelToken,
    thread: JoinHandle<()>,
}

impl Worker {
    /// Starts `request` on a new thread. `repaint` is asked to redraw the
    /// window whenever there is news, since an idle window does not redraw.
    pub(crate) fn start(request: SeparationRequest, repaint: egui::Context) -> io::Result<Self> {
        let (sender, events) = mpsc::channel();
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let thread = thread::Builder::new()
            .name("separation".to_owned())
            .spawn(move || {
                let report = |progress| {
                    // A closed channel means the window is gone: nobody wants
                    // the result any more.
                    if sender.send(Event::Progress(progress)).is_err() {
                        token.cancel();
                    }
                    repaint.request_repaint();
                };
                let result = ModelStore::in_user_cache()
                    .map_err(JobError::from)
                    .and_then(|store| job::run(&request, &store, &report, &token));
                // If the window is gone, there is nobody left to tell.
                match sender.send(Event::Finished(Box::new(result))) {
                    Ok(()) | Err(_) => {}
                }
                repaint.request_repaint();
            })?;
        Ok(Self {
            events,
            cancel,
            thread,
        })
    }

    /// Asks the job to stop. It does so within one model run and then
    /// reports [`Outcome::Cancelled`].
    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Takes everything the job has reported since the last call, without
    /// waiting.
    pub(crate) fn poll(&self) -> Polled {
        let mut polled = Polled {
            progress: None,
            outcome: None,
        };
        loop {
            match self.events.try_recv() {
                Ok(Event::Progress(progress)) => polled.progress = Some(progress),
                Ok(Event::Finished(result)) => {
                    polled.outcome = Some(outcome_of(*result));
                    return polled;
                }
                Err(TryRecvError::Empty) => return polled,
                // The thread is gone and never sent a result.
                Err(TryRecvError::Disconnected) => {
                    polled.outcome = Some(Outcome::Failed(Failure::Crashed));
                    return polled;
                }
            }
        }
    }

    /// Cancels the job and waits up to `patience` for it to wind down, so
    /// that it can remove its unfinished files before the process exits.
    pub(crate) fn stop(self, patience: Duration) {
        self.cancel.cancel();
        let deadline = Instant::now() + patience;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(remaining) {
                Ok(Event::Progress(_)) => {}
                Ok(Event::Finished(_)) | Err(RecvTimeoutError::Disconnected) => break,
                // Still busy, for instance in a stalled download. Leave it
                // behind instead of keeping the window from closing.
                Err(RecvTimeoutError::Timeout) => return,
            }
        }
        // The thread has reported and is about to return. A panic in it has
        // nowhere to be shown any more.
        match self.thread.join() {
            Ok(()) | Err(_) => {}
        }
    }
}

fn outcome_of(result: Result<JobEnd, JobError>) -> Outcome {
    match result {
        Ok(JobEnd::Completed(output)) => Outcome::Completed(output),
        Ok(JobEnd::Cancelled) => Outcome::Cancelled,
        Err(error) => Outcome::Failed(Failure::Job(error)),
    }
}
