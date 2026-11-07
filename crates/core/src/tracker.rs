//! What has become of each file of a running batch, put together from what
//! the worker reports.
//!
//! Like `timing`, nothing here reads the clock: every method is given the
//! current moment.

use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::{
    batch::Batch,
    domain::ModelId,
    job::{JobEnd, Progress, SeparationOutput},
    model::DownloadProgress,
    separate::ActiveDevice,
};

use crate::{
    timing::{self, Clock, Left, Pace, Timing},
    worker::{Channel, Event, Failure},
};

#[derive(Debug)]
enum State {
    Waiting,
    Downloading {
        progress: stem_split_media_fetching::Progress,
        started: Instant,
    },
    Downloaded {
        took: Duration,
    },
    Running {
        timing: Timing,
        progress: Option<Progress>,
        /// How many model runs it takes, once it has said.
        runs: Option<u64>,
        /// How its separation has gone, as of its latest model run.
        pace: Option<Pace>,
    },
    Completed {
        output: SeparationOutput,
        took: Duration,
        runs: Option<u64>,
        pace: Option<Pace>,
    },
    Failed {
        failure: Failure,
        took: Duration,
    },
    Cancelled,
}

/// A file of the batch: where it comes from and where its stems go.
#[derive(Debug)]
struct File {
    input: PathBuf,
    folder: PathBuf,
    state: State,
    /// How many model runs it should take, from its stated length.
    expected: Option<u64>,
}

/// The model download that comes before the files.
#[derive(Debug)]
struct Download {
    progress: DownloadProgress,
    timing: Timing,
}

/// What a row of the progress screen shows about its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    Waiting,
    /// `None` until the job has said what it is doing.
    Working(Option<Progress>),
    Downloading(stem_split_media_fetching::Progress),
    Downloaded,
    Completed(ActiveDevice),
    Failed,
    Cancelled,
}

#[derive(Debug)]
pub struct Row<'a> {
    pub input: &'a Path,
    pub status: RowStatus,
    pub failure: Option<&'a Failure>,
    /// The file's own time; `None` before it starts and once cancelled.
    pub clock: Option<Clock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadRow {
    pub model: ModelId,
    pub progress: DownloadProgress,
    pub clock: Clock,
}

/// Everything the progress screen shows.
#[derive(Debug)]
pub struct Overview<'a> {
    pub download: Option<DownloadRow>,
    pub rows: Vec<Row<'a>>,
    /// The batch as a whole, from the moment it was started.
    pub total: Clock,
}

/// How one file of a finished batch ended.
#[derive(Debug)]
pub enum Outcome {
    Downloaded,
    Completed {
        output: SeparationOutput,
        took: Duration,
    },
    Failed(Failure),
    /// Stopped by the user, or never started because of that.
    Cancelled,
}

#[derive(Debug)]
pub struct FileResult {
    pub input: PathBuf,
    pub folder: PathBuf,
    pub outcome: Outcome,
}

/// Every file of a finished batch, in the order they were chosen.
#[derive(Debug)]
pub struct Results {
    pub files: Vec<FileResult>,
    /// From the start of the batch to its end.
    pub took: Duration,
}

impl Results {
    /// The folder all results went to, if they all went to one.
    #[must_use]
    pub fn common_folder(&self) -> Option<&Path> {
        let mut folders = self.files.iter().map(|file| file.folder.as_path());
        let first = folders.next()?;
        folders.all(|folder| folder == first).then_some(first)
    }

    #[must_use]
    pub fn completed(&self) -> usize {
        self.files
            .iter()
            .filter(|file| {
                matches!(
                    file.outcome,
                    Outcome::Completed { .. } | Outcome::Downloaded
                )
            })
            .count()
    }
}

/// How a batch ended.
#[derive(Debug)]
pub enum End {
    /// The model could not be fetched, so no file was started.
    NotFetched(Failure),
    /// Every file was cancelled: there is nothing to show.
    Cancelled,
    Results(Results),
}

/// The state of a running batch.
#[derive(Debug)]
pub struct Tracker {
    started: Instant,
    model: ModelId,
    download: Option<Download>,
    fetch_failure: Option<Failure>,
    files: Vec<File>,
    /// When the first file started; `None` while the model downloads.
    first_start: Option<Instant>,
    /// How many files run at a time.
    lanes: NonZeroUsize,
}

fn count(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

impl Tracker {
    #[must_use]
    pub fn for_url(
        model: ModelId,
        info: &stem_split_media_fetching::MediaInfo,
        folder: &Path,
        now: Instant,
    ) -> Self {
        Self {
            started: now,
            model,
            download: None,
            fetch_failure: None,
            files: info
                .items()
                .map(|item| File {
                    input: PathBuf::from(item.title()),
                    folder: folder.to_path_buf(),
                    state: State::Waiting,
                    expected: None,
                })
                .collect(),
            first_start: None,
            lanes: NonZeroUsize::MIN,
        }
    }

    #[must_use]
    pub fn new(batch: &Batch, now: Instant) -> Self {
        Self {
            started: now,
            model: batch.model(),
            download: None,
            fetch_failure: None,
            files: batch
                .requests()
                .map(|request| File {
                    input: request.input().path().to_path_buf(),
                    folder: request.output().path().to_path_buf(),
                    state: State::Waiting,
                    expected: None,
                })
                .collect(),
            first_start: None,
            lanes: batch.parallel_jobs(),
        }
    }

    pub fn name_sources<'a>(&mut self, sources: impl Iterator<Item = &'a Path>) {
        for (file, source) in self.files.iter_mut().zip(sources) {
            file.input = source.to_path_buf();
        }
    }

    fn source_progress(
        &mut self,
        index: usize,
        progress: stem_split_media_fetching::Progress,
        now: Instant,
    ) {
        self.first_start.get_or_insert(now);
        if let Some(file) = self.files.get_mut(index) {
            let started = match file.state {
                State::Downloading { started, .. } => started,
                State::Waiting
                | State::Downloaded { .. }
                | State::Running { .. }
                | State::Completed { .. }
                | State::Failed { .. }
                | State::Cancelled => now,
            };
            file.state = State::Downloading { progress, started };
        }
    }

    fn source_finished(&mut self, index: usize, result: Result<PathBuf, Failure>, now: Instant) {
        if let Some(file) = self.files.get_mut(index) {
            let took = match file.state {
                State::Downloading { started, .. } => now.saturating_duration_since(started),
                State::Waiting
                | State::Downloaded { .. }
                | State::Running { .. }
                | State::Completed { .. }
                | State::Failed { .. }
                | State::Cancelled => Duration::ZERO,
            };
            file.state = match result {
                Ok(path) => {
                    file.input = path;
                    State::Downloaded { took }
                }
                Err(failure) => State::Failed { failure, took },
            };
        }
    }

    /// Takes in an event that arrived at `now`.
    pub fn apply(&mut self, event: Event, now: Instant) {
        match event {
            Event::SourceProgress(index, progress) => self.source_progress(index, progress, now),
            Event::SourceFinished(index, result) => self.source_finished(index, result, now),
            // Consumed by the screen transition before tracking begins.
            Event::SourceReady(_, _) => {}
            Event::Expected(index, runs) => {
                if let Some(file) = self.files.get_mut(index) {
                    file.expected = Some(count(runs.get()));
                }
            }
            Event::Fetching(progress) => {
                let download = self.download.get_or_insert_with(|| Download {
                    progress,
                    timing: Timing::starting_at(now),
                });
                download.progress = progress;
                download
                    .timing
                    .observe(Progress::FetchingModel(progress), now);
            }
            Event::FetchFailed(failure) => {
                self.download = None;
                self.fetch_failure = Some(failure);
            }
            Event::Started(index) => {
                self.download = None;
                self.first_start.get_or_insert(now);
                if let Some(file) = self.files.get_mut(index) {
                    file.state = State::Running {
                        timing: Timing::starting_at(now),
                        progress: None,
                        runs: None,
                        pace: None,
                    };
                }
            }
            Event::Progress(index, progress) => {
                let Some(File {
                    state:
                        State::Running {
                            timing,
                            progress: latest,
                            runs,
                            pace,
                        },
                    expected,
                    ..
                }) = self.files.get_mut(index)
                else {
                    return;
                };
                timing.observe(progress, now);
                *latest = Some(progress);
                if let Progress::Separating { total, .. } = progress {
                    // The separation counts its own runs only; the stated
                    // length also covers the cleanup windows that follow.
                    let separation = count(total.get());
                    *runs = Some(expected.map_or(separation, |expected| expected.max(separation)));
                    *pace = timing.pace();
                }
            }
            Event::Finished(index, outcome) => {
                let Some(file) = self.files.get_mut(index) else {
                    return;
                };
                let (took, runs, pace) = match &file.state {
                    State::Running {
                        timing, runs, pace, ..
                    } => (timing.elapsed(now), *runs, *pace),
                    // Never started: its thread could not be created.
                    State::Waiting
                    | State::Downloading { .. }
                    | State::Downloaded { .. }
                    | State::Completed { .. }
                    | State::Failed { .. }
                    | State::Cancelled => (Duration::ZERO, None, None),
                };
                file.state = match outcome {
                    Ok(JobEnd::Completed(output)) => State::Completed {
                        output,
                        took,
                        runs,
                        pace,
                    },
                    Ok(JobEnd::Cancelled) => State::Cancelled,
                    Err(failure) => State::Failed { failure, took },
                };
            }
        }
    }

    /// What the progress screen shows at `now`.
    #[must_use]
    pub fn overview(&self, now: Instant) -> Overview<'_> {
        let download = self.download.as_ref().map(|download| DownloadRow {
            model: self.model,
            progress: download.progress,
            clock: Clock {
                elapsed: download.timing.elapsed(now),
                remaining: download.timing.remaining(now),
            },
        });
        let rows = self
            .files
            .iter()
            .map(|file| {
                let (status, clock) = match &file.state {
                    State::Waiting => (RowStatus::Waiting, None),
                    State::Downloading { progress, started } => (
                        RowStatus::Downloading(*progress),
                        Some(Clock {
                            elapsed: now.saturating_duration_since(*started),
                            remaining: None,
                        }),
                    ),
                    State::Downloaded { took } => (
                        RowStatus::Downloaded,
                        Some(Clock {
                            elapsed: *took,
                            remaining: None,
                        }),
                    ),
                    State::Running {
                        timing, progress, ..
                    } => (
                        RowStatus::Working(*progress),
                        Some(Clock {
                            elapsed: timing.elapsed(now),
                            remaining: timing.remaining(now),
                        }),
                    ),
                    State::Completed { output, took, .. } => (
                        RowStatus::Completed(output.device()),
                        Some(Clock {
                            elapsed: *took,
                            remaining: None,
                        }),
                    ),
                    State::Failed { took, .. } => (
                        RowStatus::Failed,
                        Some(Clock {
                            elapsed: *took,
                            remaining: None,
                        }),
                    ),
                    State::Cancelled => (RowStatus::Cancelled, None),
                };
                Row {
                    input: &file.input,
                    status,
                    failure: match &file.state {
                        State::Failed { failure, .. } => Some(failure),
                        State::Waiting
                        | State::Downloading { .. }
                        | State::Downloaded { .. }
                        | State::Running { .. }
                        | State::Completed { .. }
                        | State::Cancelled => None,
                    },
                    clock,
                }
            })
            .collect();
        Overview {
            download,
            rows,
            total: Clock {
                elapsed: now.saturating_duration_since(self.started),
                remaining: self.remaining(now),
            },
        }
    }

    /// The estimate for the whole batch.
    fn remaining(&self, now: Instant) -> Option<Duration> {
        let paces: Vec<Pace> = self
            .files
            .iter()
            .filter_map(|file| match &file.state {
                State::Running { pace, .. } | State::Completed { pace, .. } => *pace,
                State::Waiting
                | State::Downloading { .. }
                | State::Downloaded { .. }
                | State::Failed { .. }
                | State::Cancelled => None,
            })
            .collect();
        // A file whose length is not known is taken to be as long as the
        // average of those whose length is.
        let known: Vec<u64> = self
            .files
            .iter()
            .filter_map(|file| match &file.state {
                State::Running { runs, .. } | State::Completed { runs, .. } => {
                    runs.or(file.expected)
                }
                State::Waiting => file.expected,
                State::Downloading { .. }
                | State::Downloaded { .. }
                | State::Failed { .. }
                | State::Cancelled => None,
            })
            .collect();
        let average = u64::try_from(known.len())
            .ok()
            .filter(|&count| count > 0)
            .map(|count| {
                known
                    .iter()
                    .fold(0_u64, |sum, runs| sum.saturating_add(*runs))
                    / count
            });

        // Running files hold their lanes; waiting ones follow in order.
        let mut left = Vec::with_capacity(self.files.len());
        for file in &self.files {
            if let State::Running { timing, runs, .. } = &file.state {
                left.push(match timing.remaining(now) {
                    Some(remaining) => Left::Measured(remaining),
                    None => Left::Runs {
                        runs: runs.or(file.expected).or(average)?,
                        elapsed: timing.elapsed(now),
                    },
                });
            }
        }
        for file in &self.files {
            if let State::Waiting = file.state {
                left.push(Left::Runs {
                    runs: file.expected.or(average)?,
                    elapsed: Duration::ZERO,
                });
            }
        }
        timing::batch_remaining(&left, &paces, self.lanes)
    }

    /// How the batch ended, once the worker has nothing more to say.
    /// `channel` tells whether it ended in an orderly way.
    #[must_use]
    pub fn finish(self, channel: Channel, now: Instant) -> End {
        if let Some(failure) = self.fetch_failure {
            return End::NotFetched(failure);
        }
        let lost = match channel {
            Channel::Lost => true,
            Channel::Open | Channel::Ended => false,
        };
        if lost && self.first_start.is_none() {
            return End::NotFetched(Failure::Crashed);
        }
        let files: Vec<FileResult> = self
            .files
            .into_iter()
            .map(|file| {
                let outcome = match file.state {
                    State::Completed { output, took, .. } => Outcome::Completed { output, took },
                    State::Downloaded { .. } => Outcome::Downloaded,
                    State::Failed { failure, .. } => Outcome::Failed(failure),
                    State::Cancelled => Outcome::Cancelled,
                    // Left out by a cancellation, unless the worker died.
                    State::Waiting | State::Downloading { .. } if !lost => Outcome::Cancelled,
                    // A job that never reported its end.
                    State::Waiting | State::Downloading { .. } | State::Running { .. } => {
                        Outcome::Failed(Failure::Crashed)
                    }
                };
                FileResult {
                    input: file.input,
                    folder: file.folder,
                    outcome,
                }
            })
            .collect();
        if files
            .iter()
            .all(|file| matches!(file.outcome, Outcome::Cancelled))
        {
            return End::Cancelled;
        }
        End::Results(Results {
            files,
            took: now.saturating_duration_since(self.started),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs};

    use crate::{
        domain::{ByteCount, InputFile, InputFiles, OutputChoice, OverwritePolicy},
        job::OutputKind,
        separate::DevicePreference,
    };

    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn downloads_keep_independent_progress_and_file_failures() -> Result<(), Box<dyn Error>> {
        use stem_split_media_fetching::{MediaInfo, MediaItem, MediaUrl};
        let directory = tempfile::tempdir()?;
        let url = MediaUrl::new("https://example.org/item")?;
        let info = MediaInfo::Playlist {
            title: "List".to_owned(),
            first: MediaItem::new("First".to_owned(), url.clone()),
            rest: vec![MediaItem::new("Second".to_owned(), url)],
        };
        let now = Instant::now();
        let mut tracker = Tracker::for_url(ModelId::Htdemucs, &info, directory.path(), now);
        tracker.apply(
            Event::SourceProgress(
                0,
                stem_split_media_fetching::Progress::Downloading {
                    downloaded: 50,
                    total: None,
                },
            ),
            now,
        );
        assert!(matches!(
            statuses(&tracker, now).as_slice(),
            [RowStatus::Downloading(_), RowStatus::Waiting]
        ));
        tracker.apply(
            Event::SourceFinished(
                0,
                Err(Failure::Media(
                    stem_split_media_fetching::DownloadError::LiveStream,
                )),
            ),
            now + SECOND,
        );
        tracker.apply(
            Event::SourceProgress(1, stem_split_media_fetching::Progress::Inspecting),
            now + SECOND,
        );
        assert!(matches!(
            statuses(&tracker, now).as_slice(),
            [RowStatus::Failed, RowStatus::Downloading(_)]
        ));
        tracker.apply(
            Event::SourceFinished(1, Ok(directory.path().join("second.webm"))),
            now + 2 * SECOND,
        );
        let End::Results(results) = tracker.finish(Channel::Ended, now + 2 * SECOND) else {
            return Err("missing per-file results".into());
        };
        assert!(matches!(
            results.files.as_slice(),
            [
                FileResult {
                    outcome: Outcome::Failed(_),
                    ..
                },
                FileResult {
                    outcome: Outcome::Downloaded,
                    ..
                }
            ]
        ));
        Ok(())
    }

    fn tracker_for(
        directory: &Path,
        count: usize,
        now: Instant,
    ) -> Result<Tracker, Box<dyn Error>> {
        let mut files = Vec::new();
        for index in 0..count {
            let path = directory.join(format!("song {index}.mp3"));
            fs::write(&path, b"")?;
            files.push(InputFile::new(path)?);
        }
        let batch = Batch::new(
            &InputFiles::new(files).ok_or("no files")?,
            &OutputChoice::BesideInput,
            ModelId::Htdemucs,
            DevicePreference::Cpu,
            OverwritePolicy::Refuse,
            OutputKind::STEMS,
        )?;
        Ok(Tracker::new(&batch, now))
    }

    fn separating(done: usize, total: usize) -> Progress {
        Progress::Separating {
            done,
            total: NonZeroUsize::new(total).unwrap_or(NonZeroUsize::MIN),
            device: ActiveDevice::DirectMl,
        }
    }

    fn statuses(tracker: &Tracker, now: Instant) -> Vec<RowStatus> {
        tracker
            .overview(now)
            .rows
            .iter()
            .map(|row| row.status)
            .collect()
    }

    #[test]
    fn rows_follow_their_files() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let start = Instant::now();
        let mut tracker = tracker_for(directory.path(), 3, start)?;
        assert_eq!(
            statuses(&tracker, start),
            [RowStatus::Waiting, RowStatus::Waiting, RowStatus::Waiting]
        );

        tracker.apply(Event::Started(0), start + SECOND);
        tracker.apply(Event::Started(1), start + SECOND);
        tracker.apply(Event::Progress(1, Progress::Decoding), start + 2 * SECOND);
        tracker.apply(
            Event::Finished(0, Err(Failure::Crashed)),
            start + 4 * SECOND,
        );

        let overview = tracker.overview(start + 5 * SECOND);
        let rows: Vec<(RowStatus, Option<Duration>)> = overview
            .rows
            .iter()
            .map(|row| (row.status, row.clock.map(|clock| clock.elapsed)))
            .collect();
        assert_eq!(
            rows,
            [
                (RowStatus::Failed, Some(3 * SECOND)),
                (
                    RowStatus::Working(Some(Progress::Decoding)),
                    Some(4 * SECOND)
                ),
                (RowStatus::Waiting, None),
            ]
        );
        assert_eq!(overview.total.elapsed, 5 * SECOND);
        Ok(())
    }

    #[test]
    fn download_row_lasts_until_the_first_file_starts() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let start = Instant::now();
        let mut tracker = tracker_for(directory.path(), 2, start)?;
        let progress = DownloadProgress::new(ByteCount::new(10), ByteCount::new(100));

        tracker.apply(Event::Fetching(progress), start + SECOND);
        let download = tracker.overview(start + 3 * SECOND).download;
        assert_eq!(
            download.map(|row| (row.progress, row.clock.elapsed)),
            Some((progress, 2 * SECOND))
        );

        tracker.apply(Event::Started(0), start + 4 * SECOND);
        assert_eq!(tracker.overview(start + 4 * SECOND).download, None);
        Ok(())
    }

    /// Starts files 0 and 1 at 100 s, after a download, and has them
    /// report their first runs at 103 s and more by 108 s: a lead of 3 s
    /// each, 8 runs after the first in 10 s.
    fn two_running(tracker: &mut Tracker, start: Instant) {
        tracker.apply(Event::Started(0), start + 100 * SECOND);
        tracker.apply(Event::Started(1), start + 100 * SECOND);
        tracker.apply(Event::Progress(0, separating(1, 10)), start + 103 * SECOND);
        tracker.apply(Event::Progress(1, separating(1, 10)), start + 103 * SECOND);
        tracker.apply(Event::Progress(0, separating(6, 10)), start + 108 * SECOND);
        tracker.apply(Event::Progress(1, separating(4, 10)), start + 108 * SECOND);
    }

    #[test]
    fn whole_batch_is_laid_out_on_the_lanes() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let start = Instant::now();
        let mut tracker = tracker_for(directory.path(), 3, start)?;
        tracker.apply(Event::Started(0), start + 100 * SECOND);
        assert_eq!(tracker.overview(start + 101 * SECOND).total.remaining, None);

        two_running(&mut tracker, start);

        // File 0 needs 4 s more, file 1 10 s. The waiting one is taken to
        // need 10 runs like them: 3 s + 9 runs at 1.25 s = 14.25 s, on the
        // lane of file 0.
        let total = tracker.overview(start + 108 * SECOND).total;
        assert_eq!(total.remaining, Some(Duration::from_millis(18_250)));
        assert_eq!(total.elapsed, 108 * SECOND);
        Ok(())
    }

    #[test]
    fn stated_lengths_of_waiting_files_count_in_the_estimate() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let start = Instant::now();
        let mut tracker = tracker_for(directory.path(), 3, start)?;
        // The waiting file is short: 2 runs, 4.25 s after the 4 s lane.
        let runs = NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN);
        tracker.apply(Event::Expected(2, runs), start);

        two_running(&mut tracker, start);

        // The longest lane is file 1 with 10 s.
        assert_eq!(
            tracker.overview(start + 108 * SECOND).total.remaining,
            Some(10 * SECOND)
        );
        Ok(())
    }

    #[test]
    fn finished_batch_lists_every_outcome() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let start = Instant::now();
        let mut tracker = tracker_for(directory.path(), 3, start)?;
        tracker.apply(Event::Started(0), start);
        tracker.apply(Event::Finished(0, Err(Failure::Crashed)), start + SECOND);
        tracker.apply(Event::Started(1), start + SECOND);
        tracker.apply(
            Event::Finished(1, Ok(JobEnd::Cancelled)),
            start + 2 * SECOND,
        );

        let End::Results(results) = tracker.finish(Channel::Ended, start + 3 * SECOND) else {
            return Err("expected results".into());
        };

        let outcomes: Vec<&str> = results
            .files
            .iter()
            .map(|file| match file.outcome {
                Outcome::Downloaded => "downloaded",
                Outcome::Completed { .. } => "completed",
                Outcome::Failed(_) => "failed",
                Outcome::Cancelled => "cancelled",
            })
            .collect();
        assert_eq!(outcomes, ["failed", "cancelled", "cancelled"]);
        assert_eq!(results.took, 3 * SECOND);
        assert_eq!(results.completed(), 0);
        assert_eq!(results.common_folder(), Some(directory.path()));
        Ok(())
    }

    #[test]
    fn batch_cancelled_before_any_result_ends_with_nothing_to_show() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let start = Instant::now();
        let mut tracker = tracker_for(directory.path(), 3, start)?;
        tracker.apply(Event::Started(0), start);
        tracker.apply(Event::Finished(0, Ok(JobEnd::Cancelled)), start + SECOND);

        assert!(matches!(
            tracker.finish(Channel::Ended, start + SECOND),
            End::Cancelled
        ));
        Ok(())
    }

    #[test]
    fn failed_download_or_a_lost_worker_before_any_file_is_a_batch_failure()
    -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let start = Instant::now();
        let mut failed = tracker_for(directory.path(), 2, start)?;
        failed.apply(Event::FetchFailed(Failure::Crashed), start);
        let lost = tracker_for(directory.path(), 2, start)?;

        assert!(matches!(
            failed.finish(Channel::Ended, start),
            End::NotFetched(Failure::Crashed)
        ));
        assert!(matches!(
            lost.finish(Channel::Lost, start),
            End::NotFetched(Failure::Crashed)
        ));
        Ok(())
    }

    #[test]
    fn lost_worker_fails_the_files_it_left_unfinished() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let start = Instant::now();
        let mut tracker = tracker_for(directory.path(), 2, start)?;
        tracker.apply(Event::Started(0), start);

        let End::Results(results) = tracker.finish(Channel::Lost, start) else {
            return Err("expected results".into());
        };
        assert!(
            results
                .files
                .iter()
                .all(|file| matches!(file.outcome, Outcome::Failed(Failure::Crashed)))
        );
        Ok(())
    }
}
