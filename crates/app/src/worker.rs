//! Running a batch of separation jobs off the interface thread.
//!
//! One thread leads the batch. It makes sure the model is on disk, once for
//! all files, since jobs downloading the same file at the same time would
//! write over each other. Then it starts the jobs, each on a thread of its
//! own, never more at once than the batch allows, and the next one as soon as
//! one ends. Everything is reported through a single channel.

use std::{
    io,
    num::NonZeroUsize,
    panic::{self, AssertUnwindSafe},
    sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use eframe::egui;
use stem_split_core::{
    CancelToken,
    batch::Batch,
    job::{self, JobEnd, JobError, Progress, ProgressSink, SeparationRequest},
    model::{DownloadProgress, Ensured, ModelStore},
};

/// Why a file was not split.
#[derive(Debug)]
pub(crate) enum Failure {
    Job(JobError),
    /// A thread could not be created.
    NotStarted(io::Error),
    /// A thread ended without reporting a result, which means a library it
    /// uses panicked.
    Crashed,
}

/// What the worker reports. The number is the position of the file in the
/// batch.
#[derive(Debug)]
pub(crate) enum Event {
    /// How many model runs a file will take, as far as its stated length
    /// tells; reported for every file whose length is known, before any
    /// file starts.
    Expected(usize, NonZeroUsize),
    /// The model is being downloaded; no file has started yet.
    Fetching(DownloadProgress),
    /// The model could not be made available, so no file was started.
    FetchFailed(Failure),
    Started(usize),
    Progress(usize, Progress),
    Finished(usize, Result<JobEnd, Failure>),
}

/// What travels through the channel: the events, then a last word.
enum Message {
    Event(Event),
    /// Nothing more will come. Files that were never started were left out
    /// because the batch was cancelled or its model could not be fetched.
    Ended,
}

/// Whether the worker has more to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channel {
    Open,
    Ended,
    /// The leading thread is gone without its last word: it panicked.
    Lost,
}

/// What arrived from the worker since the last look.
#[derive(Debug)]
pub(crate) struct Polled {
    pub(crate) events: Vec<Event>,
    pub(crate) channel: Channel,
}

/// How fetching the model ended, short of a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fetched {
    Ready,
    Cancelled,
}

/// A batch running on its own threads.
#[derive(Debug)]
pub(crate) struct Worker {
    messages: Receiver<Message>,
    cancel: CancelToken,
    thread: JoinHandle<()>,
}

impl Worker {
    /// Starts `batch`. `repaint` is asked to redraw the window whenever
    /// there is news, since an idle window does not redraw.
    pub(crate) fn start(batch: Batch, repaint: egui::Context) -> io::Result<Self> {
        let (sender, messages) = mpsc::channel();
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let thread = thread::Builder::new()
            .name("batch".to_owned())
            .spawn(move || {
                let report = |event| {
                    // A closed channel means the window is gone: nobody wants
                    // the results any more.
                    if sender.send(Message::Event(event)).is_err() {
                        token.cancel();
                    }
                    repaint.request_repaint();
                };
                // Reading the headers takes a moment per file; the
                // estimate for the whole batch is better for it.
                for (index, request) in batch.requests().enumerate() {
                    if let Some(runs) = job::expected_runs(request) {
                        report(Event::Expected(index, runs));
                    }
                }
                match ModelStore::in_user_cache() {
                    Ok(store) => run_batch(
                        &batch,
                        |on_progress| fetch(&store, &batch, on_progress, &token),
                        &|request, sink| job::run(request, &store, sink, &token),
                        &report,
                        &token,
                    ),
                    Err(error) => report(Event::FetchFailed(Failure::Job(error.into()))),
                }
                // If the window is gone, there is nobody left to tell.
                match sender.send(Message::Ended) {
                    Ok(()) | Err(_) => {}
                }
                repaint.request_repaint();
            })?;
        Ok(Self {
            messages,
            cancel,
            thread,
        })
    }

    /// Asks every job to stop. Running ones do so within one model run;
    /// waiting ones are not started.
    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Takes everything reported since the last call, without waiting.
    pub(crate) fn poll(&self) -> Polled {
        let mut events = Vec::new();
        loop {
            match self.messages.try_recv() {
                Ok(Message::Event(event)) => events.push(event),
                Ok(Message::Ended) => {
                    return Polled {
                        events,
                        channel: Channel::Ended,
                    };
                }
                Err(TryRecvError::Empty) => {
                    return Polled {
                        events,
                        channel: Channel::Open,
                    };
                }
                Err(TryRecvError::Disconnected) => {
                    return Polled {
                        events,
                        channel: Channel::Lost,
                    };
                }
            }
        }
    }

    /// Cancels the batch and waits up to `patience` for it to wind down, so
    /// that the jobs can remove their unfinished files before the process
    /// exits.
    pub(crate) fn stop(self, patience: Duration) {
        self.cancel.cancel();
        let deadline = Instant::now() + patience;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.messages.recv_timeout(remaining) {
                Ok(Message::Event(_)) => {}
                Ok(Message::Ended) | Err(RecvTimeoutError::Disconnected) => break,
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

/// Makes sure the model of `batch` is on disk.
fn fetch(
    store: &ModelStore,
    batch: &Batch,
    on_progress: &mut dyn FnMut(DownloadProgress),
    cancel: &CancelToken,
) -> Result<Fetched, Failure> {
    match store.ensure(batch.model(), on_progress, cancel) {
        Ok(Ensured::Ready(_)) => Ok(Fetched::Ready),
        Ok(Ensured::Cancelled) => Ok(Fetched::Cancelled),
        Err(error) => Err(Failure::Job(error.into())),
    }
}

/// Fetches the model with `fetch`, then runs `job` for every file of
/// `batch`, [`Batch::parallel_jobs`] at a time, in the order of the batch.
/// Stops starting files once `cancel` is set, and returns when every started
/// job has ended.
fn run_batch<F, J>(
    batch: &Batch,
    fetch: F,
    job: &J,
    report: &(dyn Fn(Event) + Sync),
    cancel: &CancelToken,
) where
    F: FnOnce(&mut dyn FnMut(DownloadProgress)) -> Result<Fetched, Failure>,
    J: Fn(&SeparationRequest, &dyn ProgressSink) -> Result<JobEnd, JobError> + Sync,
{
    match fetch(&mut |download| report(Event::Fetching(download))) {
        Ok(Fetched::Ready) => {}
        Ok(Fetched::Cancelled) => return,
        Err(failure) => {
            report(Event::FetchFailed(failure));
            return;
        }
    }

    let parallel = batch.parallel_jobs().get();
    thread::scope(|scope| {
        // Each job says here that its place is free.
        let (free, freed) = mpsc::channel::<()>();
        let mut waiting = batch.requests().enumerate();
        let mut running = 0_usize;
        loop {
            while running < parallel && !cancel.is_cancelled() {
                let Some((index, request)) = waiting.next() else {
                    break;
                };
                let free = free.clone();
                // Told here, not by the job's thread, so that files are
                // reported started in the order they are started.
                report(Event::Started(index));
                let spawned = thread::Builder::new()
                    .name(format!("separation of file {index}"))
                    .spawn_scoped(scope, move || {
                        let sink = |progress| report(Event::Progress(index, progress));
                        // A panic in a library ends this file, not the batch.
                        let result = panic::catch_unwind(AssertUnwindSafe(|| job(request, &sink)));
                        let outcome = match result {
                            Ok(Ok(end)) => Ok(end),
                            Ok(Err(error)) => Err(Failure::Job(error)),
                            Err(_) => Err(Failure::Crashed),
                        };
                        report(Event::Finished(index, outcome));
                        // The leading thread waits for this as long as it runs.
                        match free.send(()) {
                            Ok(()) | Err(_) => {}
                        }
                    });
                match spawned {
                    Ok(_) => running = running.saturating_add(1),
                    Err(error) => report(Event::Finished(index, Err(Failure::NotStarted(error)))),
                }
            }
            if running == 0 {
                break;
            }
            match freed.recv() {
                Ok(()) => running = running.saturating_sub(1),
                // Cannot happen while `free` is held here; nothing to wait for.
                Err(_) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        fs,
        path::Path,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use stem_split_core::{
        domain::{InputFile, InputFiles, ModelId, OutputChoice, OverwritePolicy},
        separate::DevicePreference,
    };

    use super::*;

    fn batch_of(directory: &Path, count: usize) -> Result<Batch, Box<dyn Error>> {
        let mut files = Vec::new();
        for index in 0..count {
            let path = directory.join(format!("song {index}.mp3"));
            fs::write(&path, b"")?;
            files.push(InputFile::new(path)?);
        }
        let inputs = InputFiles::new(files).ok_or("no files")?;
        Ok(Batch::new(
            &inputs,
            &OutputChoice::BesideInput,
            ModelId::Htdemucs,
            DevicePreference::Cpu,
            OverwritePolicy::Refuse,
        )?)
    }

    /// Runs `batch` with `job` after a fetch that reports `fetch`, and
    /// returns the events in the order they were reported.
    fn events_of<J>(
        batch: &Batch,
        fetch: Result<Fetched, Failure>,
        job: &J,
        cancel: &CancelToken,
    ) -> Vec<Event>
    where
        J: Fn(&SeparationRequest, &dyn ProgressSink) -> Result<JobEnd, JobError> + Sync,
    {
        let (sender, receiver) = mpsc::channel();
        let report = |event| match sender.send(event) {
            Ok(()) | Err(_) => {}
        };
        run_batch(batch, |_| fetch, job, &report, cancel);
        receiver.try_iter().collect()
    }

    fn started(events: &[Event]) -> Vec<usize> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Started(index) => Some(*index),
                Event::Expected(..)
                | Event::Fetching(_)
                | Event::FetchFailed(_)
                | Event::Progress(..)
                | Event::Finished(..) => None,
            })
            .collect()
    }

    fn finished(events: &[Event]) -> Vec<usize> {
        let mut indices: Vec<usize> = events
            .iter()
            .filter_map(|event| match event {
                Event::Finished(index, _) => Some(*index),
                Event::Expected(..)
                | Event::Fetching(_)
                | Event::FetchFailed(_)
                | Event::Started(_)
                | Event::Progress(..) => None,
            })
            .collect();
        indices.sort_unstable();
        indices
    }

    #[test]
    fn every_file_runs_and_never_more_than_two_at_once() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let batch = batch_of(directory.path(), 5)?;
        let running = AtomicUsize::new(0);
        let most = AtomicUsize::new(0);
        let job = |_: &SeparationRequest, sink: &dyn ProgressSink| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            sink.report(Progress::Decoding);
            thread::sleep(Duration::from_millis(30));
            running.fetch_sub(1, Ordering::SeqCst);
            Ok(JobEnd::Cancelled)
        };

        let events = events_of(&batch, Ok(Fetched::Ready), &job, &CancelToken::new());

        assert_eq!(started(&events), [0, 1, 2, 3, 4], "started in order");
        assert_eq!(finished(&events), [0, 1, 2, 3, 4]);
        assert_eq!(most.load(Ordering::SeqCst), 2);
        // Every progress report names the file it came from.
        let progress = events
            .iter()
            .filter(|event| matches!(event, Event::Progress(_, Progress::Decoding)))
            .count();
        assert_eq!(progress, 5);
        Ok(())
    }

    #[test]
    fn failed_fetch_starts_no_file() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let batch = batch_of(directory.path(), 3)?;
        let calls = AtomicUsize::new(0);
        let job = |_: &SeparationRequest, _: &dyn ProgressSink| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(JobEnd::Cancelled)
        };

        let events = events_of(&batch, Err(Failure::Crashed), &job, &CancelToken::new());

        assert!(matches!(
            events.as_slice(),
            [Event::FetchFailed(Failure::Crashed)]
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[test]
    fn cancelling_lets_running_files_end_and_starts_no_more() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let batch = batch_of(directory.path(), 6)?;
        let cancel = CancelToken::new();
        // Cancels a moment after starting, by when the second file has
        // long been started too.
        let job = |_: &SeparationRequest, _: &dyn ProgressSink| {
            thread::sleep(Duration::from_millis(20));
            cancel.cancel();
            Ok(JobEnd::Cancelled)
        };

        let events = events_of(&batch, Ok(Fetched::Ready), &job, &cancel);

        // The first two were started together; the rest never were.
        assert_eq!(started(&events), [0, 1]);
        assert_eq!(finished(&events), [0, 1]);
        Ok(())
    }

    #[test]
    fn panic_in_one_job_fails_that_file_only() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let batch = batch_of(directory.path(), 3)?;
        let job = |request: &SeparationRequest, _: &dyn ProgressSink| {
            // Stands for a library that panics.
            assert!(
                !request.input().path().ends_with("song 1.mp3"),
                "a library failed"
            );
            Ok(JobEnd::Cancelled)
        };

        let events = events_of(&batch, Ok(Fetched::Ready), &job, &CancelToken::new());

        let mut outcomes: Vec<(usize, bool)> = events
            .iter()
            .filter_map(|event| match event {
                Event::Finished(index, outcome) => {
                    Some((*index, matches!(outcome, Err(Failure::Crashed))))
                }
                Event::Expected(..)
                | Event::Fetching(_)
                | Event::FetchFailed(_)
                | Event::Started(_)
                | Event::Progress(..) => None,
            })
            .collect();
        outcomes.sort_unstable();
        assert_eq!(outcomes, [(0, false), (1, true), (2, false)]);
        Ok(())
    }
}
