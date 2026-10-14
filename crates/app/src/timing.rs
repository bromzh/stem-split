//! How long a job has been running, how long its current stage will take,
//! and how long a whole batch of jobs still needs.
//!
//! Nothing here reads the clock: every method is given the current moment, so
//! the arithmetic can be tested with made-up times.

use std::{
    num::NonZeroUsize,
    time::{Duration, Instant},
};

use stem_split_core::job::Progress;

/// What a stage counts its progress in. Only these two stages report how far
/// they are: downloading in bytes, separating in model runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Measure {
    Download,
    Separation,
}

/// How much of a stage was done at one moment.
#[derive(Debug, Clone, Copy)]
struct Reading {
    at: Instant,
    done: u64,
}

/// A measurable stage in progress.
#[derive(Debug, Clone, Copy)]
struct Stage {
    measure: Measure,
    /// The first report of the stage. The speed is taken from here on,
    /// which leaves out whatever happened before it: connecting to the
    /// server, or loading the model and warming up the device.
    first: Reading,
    latest: Reading,
    total: u64,
}

/// How long something has run and how long it should still take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Clock {
    pub(crate) elapsed: Duration,
    pub(crate) remaining: Option<Duration>,
}

/// How the separation of one file has gone so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pace {
    /// From the start of the file to its first finished model run: reading
    /// the file, loading the graph and warming up the device.
    pub(crate) lead: Duration,
    /// Model runs after the first one, and the time they took.
    pub(crate) runs: u64,
    pub(crate) spent: Duration,
}

/// What is left of one file, as the estimate for a whole batch sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Left {
    /// Running, with this much time left by its own estimate.
    Measured(Duration),
    /// Expected to take `runs` model runs in all, of which `elapsed` has
    /// passed; nothing for a file that waits.
    Runs { runs: u64, elapsed: Duration },
}

/// Estimated time until every file of a batch is split, `lanes` files at a
/// time.
///
/// A running file that estimates itself takes that long. Any other takes
/// the average lead of the files measured so far, plus its runs after the
/// first at their average pace, less the time it has already run. `files`
/// come running ones first, then waiting ones in the order they will start;
/// each is put on the lane that frees first, as the worker does, and the
/// estimate is when the last lane frees. That keeps the fixed cost of every
/// file apart from its runs, and a lone long file from being shared out
/// over lanes it cannot use.
///
/// `None` until some file has finished a run after its first.
pub(crate) fn batch_remaining(
    files: &[Left],
    paces: &[Pace],
    lanes: NonZeroUsize,
) -> Option<Duration> {
    let measured = u32::try_from(paces.len()).ok().filter(|&count| count > 0)?;
    let (lead, runs, spent) = paces.iter().fold(
        (Duration::ZERO, 0_u64, Duration::ZERO),
        |(lead, runs, spent), pace| {
            (
                lead.saturating_add(pace.lead),
                runs.saturating_add(pace.runs),
                spent.saturating_add(pace.spent),
            )
        },
    );
    if runs == 0 {
        return None;
    }
    let lead = lead.checked_div(measured)?;
    let at_pace = |count: u64| {
        let nanoseconds = spent.as_nanos().saturating_mul(u128::from(count)) / u128::from(runs);
        Duration::from_nanos(u64::try_from(nanoseconds).unwrap_or(u64::MAX))
    };

    let mut free = vec![Duration::ZERO; lanes.get()];
    for file in files {
        let time = match *file {
            Left::Measured(remaining) => remaining,
            Left::Runs { runs, elapsed } => lead
                .saturating_add(at_pace(runs.saturating_sub(1)))
                .saturating_sub(elapsed),
        };
        let lane = free.iter_mut().min()?;
        *lane = lane.saturating_add(time);
    }
    free.into_iter().max()
}

/// The clock of a running job.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timing {
    started: Instant,
    stage: Option<Stage>,
}

impl Timing {
    pub(crate) const fn starting_at(now: Instant) -> Self {
        Self {
            started: now,
            stage: None,
        }
    }

    /// Takes note of a progress report that arrived at `now`.
    pub(crate) fn observe(&mut self, progress: Progress, now: Instant) {
        match progress {
            Progress::FetchingModel(download) => self.measure(
                Measure::Download,
                download.downloaded().get(),
                download.total().get(),
                now,
            ),
            Progress::Separating { done, total, .. } => self.measure(
                Measure::Separation,
                u64::try_from(done).unwrap_or(u64::MAX),
                u64::try_from(total.get()).unwrap_or(u64::MAX),
                now,
            ),
            // A graph is loaded before every pass. Between two passes the
            // separation goes on, and so does its estimate.
            Progress::LoadingModel => {
                self.stage = self.stage.filter(|stage| match stage.measure {
                    Measure::Separation => true,
                    Measure::Download => false,
                });
            }
            Progress::Decoding | Progress::Finalizing => self.stage = None,
        }
    }

    fn measure(&mut self, measure: Measure, done: u64, total: u64, now: Instant) {
        let reading = Reading { at: now, done };
        self.stage = Some(match self.stage {
            Some(stage) if stage.measure == measure => Stage {
                latest: reading,
                total,
                ..stage
            },
            Some(_) | None => Stage {
                measure,
                first: reading,
                latest: reading,
                total,
            },
        });
    }

    /// How the separation has gone so far; `None` before its first run
    /// and once it has moved on to writing the files.
    pub(crate) fn pace(&self) -> Option<Pace> {
        let stage = self
            .stage
            .filter(|stage| stage.measure == Measure::Separation)?;
        Some(Pace {
            lead: stage.first.at.saturating_duration_since(self.started),
            runs: stage.latest.done.saturating_sub(stage.first.done),
            spent: stage.latest.at.saturating_duration_since(stage.first.at),
        })
    }

    /// Time since the job started.
    pub(crate) fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started)
    }

    /// Estimated time until the current stage is finished: what is left of
    /// it, at the average speed since its first report. `None` while the
    /// stage does not measure its progress or has not advanced yet.
    pub(crate) fn remaining(&self, now: Instant) -> Option<Duration> {
        let stage = self.stage?;
        let advanced = stage
            .latest
            .done
            .checked_sub(stage.first.done)
            .filter(|&units| units > 0)?;
        let spent = stage.latest.at.saturating_duration_since(stage.first.at);
        let left = stage.total.saturating_sub(stage.latest.done);

        let nanoseconds = spent.as_nanos().saturating_mul(u128::from(left)) / u128::from(advanced);
        let at_latest = Duration::from_nanos(u64::try_from(nanoseconds).unwrap_or(u64::MAX));
        // The estimate was right at the latest report; time has passed since.
        Some(at_latest.saturating_sub(now.saturating_duration_since(stage.latest.at)))
    }
}

#[cfg(test)]
mod tests {
    use stem_split_core::{
        domain::ByteCount, job::Progress, model::DownloadProgress, separate::ActiveDevice,
    };

    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    fn separating(done: usize, total: usize) -> Progress {
        Progress::Separating {
            done,
            total: NonZeroUsize::new(total).unwrap_or(NonZeroUsize::MIN),
            device: ActiveDevice::Cpu,
        }
    }

    fn lanes(count: usize) -> NonZeroUsize {
        NonZeroUsize::new(count).unwrap_or(NonZeroUsize::MIN)
    }

    /// Two files measured: a lead of 3 s each, and 8 runs after the first
    /// in 10 s, 1.25 s a run.
    const PACES: [Pace; 2] = [
        Pace {
            lead: Duration::from_secs(3),
            runs: 5,
            spent: Duration::from_secs(5),
        },
        Pace {
            lead: Duration::from_secs(3),
            runs: 3,
            spent: Duration::from_secs(5),
        },
    ];

    #[test]
    fn batch_estimate_needs_a_measured_pace() {
        let waiting = [Left::Runs {
            runs: 10,
            elapsed: Duration::ZERO,
        }];
        assert_eq!(batch_remaining(&waiting, &[], lanes(2)), None);
        let first_run_only = [Pace {
            lead: 3 * SECOND,
            runs: 0,
            spent: Duration::ZERO,
        }];
        assert_eq!(batch_remaining(&waiting, &first_run_only, lanes(2)), None);
    }

    #[test]
    fn waiting_file_takes_the_lane_that_frees_first() {
        let files = [
            Left::Measured(4 * SECOND),
            Left::Measured(10 * SECOND),
            // 3 s of lead and 9 runs at 1.25 s: 14.25 s, after the 4 s lane.
            Left::Runs {
                runs: 10,
                elapsed: Duration::ZERO,
            },
        ];

        assert_eq!(
            batch_remaining(&files, &PACES, lanes(2)),
            Some(Duration::from_millis(18_250))
        );
    }

    #[test]
    fn lone_file_is_not_shared_out_over_idle_lanes() {
        let files = [Left::Measured(20 * SECOND)];

        assert_eq!(batch_remaining(&files, &PACES, lanes(2)), Some(20 * SECOND));
    }

    #[test]
    fn running_file_without_an_estimate_counts_what_has_passed() {
        // 3 s + 1 run: 4.25 s, of which 2 s have passed.
        let files = [Left::Runs {
            runs: 2,
            elapsed: 2 * SECOND,
        }];

        assert_eq!(
            batch_remaining(&files, &PACES, lanes(2)),
            Some(Duration::from_millis(2_250))
        );
    }

    #[test]
    fn elapsed_counts_from_the_start() {
        let start = Instant::now();
        let timing = Timing::starting_at(start);

        assert_eq!(timing.elapsed(start), Duration::ZERO);
        assert_eq!(timing.elapsed(start + 90 * SECOND), 90 * SECOND);
    }

    #[test]
    fn nothing_is_estimated_before_a_stage_advances() {
        let start = Instant::now();
        let mut timing = Timing::starting_at(start);
        assert_eq!(timing.remaining(start), None);

        timing.observe(Progress::Decoding, start + SECOND);
        timing.observe(Progress::LoadingModel, start + 2 * SECOND);
        assert_eq!(timing.remaining(start + 3 * SECOND), None);

        // One report gives a position but no speed yet.
        timing.observe(separating(1, 19), start + 5 * SECOND);
        assert_eq!(timing.remaining(start + 5 * SECOND), None);
    }

    #[test]
    fn remaining_follows_the_speed_since_the_first_report() {
        let start = Instant::now();
        let mut timing = Timing::starting_at(start);
        // The first run takes long: the model is loaded and warmed up.
        timing.observe(separating(1, 19), start + 5 * SECOND);
        timing.observe(separating(2, 19), start + 7 * SECOND);

        // One run took 2 s and 17 are left.
        assert_eq!(timing.remaining(start + 7 * SECOND), Some(34 * SECOND));

        // Three more runs at 3 s each: 11 s for 4 runs since the first report.
        timing.observe(separating(5, 19), start + 16 * SECOND);
        assert_eq!(
            timing.remaining(start + 16 * SECOND),
            Some(Duration::from_millis(38_500))
        );
    }

    #[test]
    fn remaining_counts_down_between_reports_and_stops_at_zero() {
        let start = Instant::now();
        let mut timing = Timing::starting_at(start);
        timing.observe(separating(1, 4), start + SECOND);
        timing.observe(separating(2, 4), start + 3 * SECOND);

        assert_eq!(timing.remaining(start + 3 * SECOND), Some(4 * SECOND));
        assert_eq!(timing.remaining(start + 4 * SECOND), Some(3 * SECOND));
        assert_eq!(timing.remaining(start + 7 * SECOND), Some(Duration::ZERO));
        // Running late does not turn the estimate negative.
        assert_eq!(timing.remaining(start + 60 * SECOND), Some(Duration::ZERO));
    }

    #[test]
    fn finished_stage_has_nothing_left() {
        let start = Instant::now();
        let mut timing = Timing::starting_at(start);
        timing.observe(separating(1, 3), start + SECOND);
        timing.observe(separating(3, 3), start + 3 * SECOND);

        assert_eq!(timing.remaining(start + 3 * SECOND), Some(Duration::ZERO));

        timing.observe(Progress::Finalizing, start + 3 * SECOND);
        assert_eq!(timing.remaining(start + 3 * SECOND), None);
    }

    #[test]
    fn loading_the_next_graph_keeps_the_separation_estimate() {
        let start = Instant::now();
        let mut timing = Timing::starting_at(start);
        timing.observe(separating(1, 8), start + SECOND);
        timing.observe(separating(2, 8), start + 2 * SECOND);

        timing.observe(Progress::LoadingModel, start + 2 * SECOND);

        assert_eq!(timing.remaining(start + 2 * SECOND), Some(6 * SECOND));
        // The time the load takes shows up in the average afterwards.
        timing.observe(separating(3, 8), start + 5 * SECOND);
        assert_eq!(timing.remaining(start + 5 * SECOND), Some(10 * SECOND));
    }

    fn downloading(done: u64, total: u64) -> Progress {
        Progress::FetchingModel(DownloadProgress::new(
            ByteCount::new(done),
            ByteCount::new(total),
        ))
    }

    #[test]
    fn download_is_estimated_in_bytes_and_decoding_ends_it() {
        let start = Instant::now();
        let mut timing = Timing::starting_at(start);
        timing.observe(downloading(100, 1000), start + SECOND);
        timing.observe(downloading(400, 1000), start + 4 * SECOND);

        // 300 bytes in 3 s, 600 bytes to go.
        assert_eq!(timing.remaining(start + 4 * SECOND), Some(6 * SECOND));

        timing.observe(Progress::Decoding, start + 10 * SECOND);
        assert_eq!(timing.remaining(start + 10 * SECOND), None);
    }

    #[test]
    fn separation_after_a_download_starts_a_new_measurement() {
        let start = Instant::now();
        let mut timing = Timing::starting_at(start);
        timing.observe(downloading(0, 1000), start);
        timing.observe(downloading(1000, 1000), start + 100 * SECOND);
        timing.observe(Progress::LoadingModel, start + 101 * SECOND);
        assert_eq!(timing.remaining(start + 101 * SECOND), None);

        timing.observe(separating(1, 3), start + 103 * SECOND);
        timing.observe(separating(2, 3), start + 104 * SECOND);

        // The slow download does not weigh on the separation estimate.
        assert_eq!(timing.remaining(start + 104 * SECOND), Some(SECOND));
        assert_eq!(timing.elapsed(start + 104 * SECOND), 104 * SECOND);
    }
}
