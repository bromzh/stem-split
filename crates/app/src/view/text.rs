//! Every user-visible string, for every interface language.
//!
//! Translations are picked by exhaustive `match`, so adding a [`Locale`], a
//! [`Text`] or an error variant in the core does not compile until each
//! string exists in each language.

use std::{path::Path, time::Duration};

use stem_split_core::{
    audio::{DecodeError, ResampleError, WriteError},
    domain::{ByteCount, InputFileError, ModelId, OutputDirError},
    job::{JobError, Progress},
    model::ModelStoreError,
    separate::{ActiveDevice, DevicePreference, InferenceError},
};

use crate::worker::Failure;

/// Decimal megabytes, the unit download sizes are usually quoted in.
const BYTES_PER_MEGABYTE: u64 = 1_000_000;

/// Interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Locale {
    #[default]
    En,
}

/// A user-visible string without parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Text {
    WindowTitle,
    File,
    NoFileChosen,
    Browse,
    Output,
    OutputBesideFile,
    OutputOtherFolder,
    Model,
    Device,
    Split,
    Cancel,
    Cancelling,
    Back,
    Replace,
    OpenFolder,
    AudioFiles,
    Starting,
    ExistingFilesHeading,
    DoneHeading,
    FailedHeading,
}

impl Text {
    pub(crate) const fn get(self, locale: Locale) -> &'static str {
        match locale {
            Locale::En => match self {
                Self::WindowTitle => "stem-split",
                Self::File => "File",
                Self::NoFileChosen => "No file chosen",
                Self::Browse => "Browse…",
                Self::Output => "Save to",
                Self::OutputBesideFile => "The folder of the file",
                Self::OutputOtherFolder => "Another folder",
                Self::Model => "Model",
                Self::Device => "Device",
                Self::Split => "Split",
                Self::Cancel => "Cancel",
                Self::Cancelling => "Cancelling…",
                Self::Back => "Back",
                Self::Replace => "Replace",
                Self::OpenFolder => "Open folder",
                Self::AudioFiles => "Audio files",
                Self::Starting => "Starting…",
                Self::ExistingFilesHeading => "These files already exist and will be replaced:",
                Self::DoneHeading => "Done",
                Self::FailedHeading => "The file was not split",
            },
        }
    }
}

/// Megabytes, rounded to the nearest whole one.
fn megabytes(bytes: ByteCount) -> u64 {
    bytes.get().saturating_add(BYTES_PER_MEGABYTE / 2) / BYTES_PER_MEGABYTE
}

/// One line describing a model, such as `htdemucs — 4 stems, fast`.
pub(crate) fn model_label(locale: Locale, model: ModelId) -> String {
    let name = model.name();
    let stems = model.layout().count();
    match locale {
        Locale::En => {
            let note = match model {
                ModelId::Htdemucs => "fast",
                ModelId::HtdemucsFt => "best quality, about 4x slower",
                ModelId::Htdemucs6s => "adds guitar and piano",
            };
            format!("{name} — {stems} stems, {note}")
        }
    }
}

/// Tells the user that starting will first download the model.
pub(crate) fn download_notice(locale: Locale, size: ByteCount) -> String {
    let size = megabytes(size);
    match locale {
        Locale::En => format!("The model ({size} MB) will be downloaded first."),
    }
}

pub(crate) const fn device_preference(locale: Locale, device: DevicePreference) -> &'static str {
    match locale {
        Locale::En => match device {
            DevicePreference::Auto => "Automatic",
            DevicePreference::Gpu => "Graphics card",
            DevicePreference::Cpu => "Processor",
        },
    }
}

const fn active_device(locale: Locale, device: ActiveDevice) -> &'static str {
    match locale {
        Locale::En => match device {
            ActiveDevice::DirectMl => "graphics card",
            ActiveDevice::Cpu => "processor",
        },
    }
}

/// What a running job is doing, in one line.
pub(crate) fn progress_status(locale: Locale, progress: Progress) -> String {
    match locale {
        Locale::En => match progress {
            Progress::FetchingModel(download) => format!(
                "Downloading the model: {} of {} MB",
                megabytes(download.downloaded()),
                megabytes(download.total())
            ),
            Progress::Decoding => "Reading the file…".to_owned(),
            Progress::LoadingModel => "Loading the model…".to_owned(),
            Progress::Separating {
                done,
                total,
                device,
            } => format!(
                "Splitting: part {done} of {total}, on the {}",
                active_device(locale, device)
            ),
            Progress::Finalizing => "Writing the files…".to_owned(),
        },
    }
}

/// A duration as `m:ss`, or `h:mm:ss` from an hour on.
fn clock(duration: Duration) -> String {
    let total = duration.as_secs();
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Rounded up to a whole second and never less than one: an estimate of
/// "0:00 left" next to a job that is still running would read as an error.
fn at_least_a_second(duration: Duration) -> Duration {
    let whole = Duration::from_secs(duration.as_secs());
    let rounded = if whole == duration {
        whole
    } else {
        whole.saturating_add(Duration::from_secs(1))
    };
    rounded.max(Duration::from_secs(1))
}

/// The time a job has taken so far and, when it can be estimated, the time
/// its current stage still needs.
pub(crate) fn time_status(
    locale: Locale,
    elapsed: Duration,
    remaining: Option<Duration>,
) -> String {
    let elapsed = clock(elapsed);
    match locale {
        Locale::En => match remaining {
            Some(remaining) => format!(
                "Elapsed {elapsed} · about {} left",
                clock(at_least_a_second(remaining))
            ),
            None => format!("Elapsed {elapsed}"),
        },
    }
}

/// Rounded to the nearest whole second: the way to state a final figure,
/// where 12.6 s is "13 seconds" and not "12".
fn to_the_nearest_second(duration: Duration) -> Duration {
    Duration::from_secs(
        duration
            .saturating_add(Duration::from_millis(500))
            .as_secs(),
    )
}

/// The line under the list of results: how long the job took, from the
/// moment it was started to the last file being in place, and where it
/// was computed.
pub(crate) fn finished_summary(locale: Locale, took: Duration, device: ActiveDevice) -> String {
    let took = clock(to_the_nearest_second(took));
    match locale {
        Locale::En => format!(
            "Finished in {took} on the {}.",
            active_device(locale, device)
        ),
    }
}

/// A message for the user: what happened in plain words, and the technical
/// description for a bug report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Message {
    pub(crate) summary: String,
    pub(crate) detail: Option<String>,
}

impl Message {
    fn plain(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            detail: None,
        }
    }

    fn with_detail(summary: impl Into<String>, detail: &dyn std::error::Error) -> Self {
        Self {
            summary: summary.into(),
            detail: Some(detail.to_string()),
        }
    }
}

/// The last component of `path`, the part a user recognizes a file by.
pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Why a file the user chose cannot be split.
pub(crate) fn input_problem(locale: Locale, error: &InputFileError) -> Message {
    match locale {
        Locale::En => match error {
            InputFileError::Inaccessible { path, .. } => {
                Message::with_detail(format!("\"{}\" cannot be opened.", file_name(path)), error)
            }
            InputFileError::NotAFile { path } => {
                Message::plain(format!("\"{}\" is not a file.", file_name(path)))
            }
            InputFileError::UnsupportedFormat { path } => Message::plain(format!(
                "\"{}\" is not a supported audio file. Choose an MP3, FLAC, WAV, OGG or M4A file.",
                file_name(path)
            )),
        },
    }
}

/// Why a folder the user chose cannot receive the results.
pub(crate) fn output_problem(locale: Locale, error: &OutputDirError) -> Message {
    match locale {
        Locale::En => match error {
            OutputDirError::Inaccessible { path, .. } => Message::with_detail(
                format!("The folder \"{}\" cannot be opened.", path.display()),
                error,
            ),
            OutputDirError::NotADirectory { path } => {
                Message::plain(format!("\"{}\" is not a folder.", path.display()))
            }
        },
    }
}

/// Why the results folder could not be shown.
pub(crate) fn open_folder_problem(locale: Locale, error: &std::io::Error) -> Message {
    match locale {
        Locale::En => Message::with_detail("The folder could not be opened.", error),
    }
}

/// Why a job did not produce its files.
pub(crate) fn failure(locale: Locale, failure: &Failure) -> Message {
    match failure {
        Failure::Job(error) => job_error(locale, error),
        Failure::NotStarted(error) => match locale {
            Locale::En => Message::with_detail("The work could not be started.", error),
        },
        Failure::Crashed => match locale {
            Locale::En => Message::plain(
                "The work stopped unexpectedly. Nothing was written. Please try again.",
            ),
        },
    }
}

fn job_error(locale: Locale, error: &JobError) -> Message {
    match error {
        JobError::Write(error) => write_error(locale, error),
        JobError::Model(error) => model_error(locale, error),
        JobError::Decode(error) => decode_error(locale, error),
        JobError::Resample(error) => resample_error(locale, error),
        JobError::Inference(error) => inference_error(locale, error),
    }
}

fn write_error(locale: Locale, error: &WriteError) -> Message {
    match locale {
        Locale::En => match error {
            WriteError::AlreadyExists { path } => Message::plain(format!(
                "\"{}\" already exists. Nothing was replaced.",
                file_name(path)
            )),
            WriteError::Create { path, .. } => Message::with_detail(
                format!(
                    "\"{}\" could not be created. Check that the folder allows writing.",
                    file_name(path)
                ),
                error,
            ),
            WriteError::Write { path, .. } => Message::with_detail(
                format!(
                    "\"{}\" could not be written. Check that the disk has free space.",
                    file_name(path)
                ),
                error,
            ),
            WriteError::Publish { path, .. } => Message::with_detail(
                format!(
                    "\"{}\" could not be put in place. It may be open in another program.",
                    file_name(path)
                ),
                error,
            ),
            WriteError::TooLong { path } => Message::plain(format!(
                "\"{}\" would be larger than a WAV file can be.",
                file_name(path)
            )),
        },
    }
}

fn model_error(locale: Locale, error: &ModelStoreError) -> Message {
    match locale {
        Locale::En => match error {
            ModelStoreError::NoCacheDirectory => {
                Message::plain("There is no folder to keep the model in for this user account.")
            }
            ModelStoreError::CreateDirectory { path, .. }
            | ModelStoreError::Write { path, .. }
            | ModelStoreError::Publish { path, .. } => Message::with_detail(
                format!(
                    "The model could not be saved to \"{}\". Check that the disk has free space.",
                    path.display()
                ),
                error,
            ),
            ModelStoreError::Request { .. } => Message::with_detail(
                "The model could not be downloaded. Check the internet connection and try again.",
                error,
            ),
            ModelStoreError::Transfer { .. } => Message::with_detail(
                "The download of the model was interrupted. Try again.",
                error,
            ),
            ModelStoreError::WrongSize { .. } | ModelStoreError::DigestMismatch { .. } => {
                Message::with_detail(
                    "The downloaded model is damaged and was discarded. Try again.",
                    error,
                )
            }
        },
    }
}

fn decode_error(locale: Locale, error: &DecodeError) -> Message {
    match locale {
        Locale::En => match error {
            DecodeError::Open { path, .. } => Message::with_detail(
                format!("\"{}\" could not be opened.", file_name(path)),
                error,
            ),
            DecodeError::Unrecognized { path, .. } | DecodeError::NoAudioTrack { path } => {
                Message::with_detail(
                    format!(
                        "\"{}\" is not audio this application can read.",
                        file_name(path)
                    ),
                    error,
                )
            }
            DecodeError::UnsupportedCodec { path, .. } => Message::with_detail(
                format!(
                    "\"{}\" uses an audio format this application cannot read.",
                    file_name(path)
                ),
                error,
            ),
            DecodeError::Read { path, .. }
            | DecodeError::InvalidSampleRate { path }
            | DecodeError::FormatChanged { path } => Message::with_detail(
                format!("\"{}\" is damaged and cannot be read.", file_name(path)),
                error,
            ),
            DecodeError::UnsupportedChannels { path, channels } => Message::plain(format!(
                "\"{}\" has {channels} channels. Only mono and stereo files can be split.",
                file_name(path)
            )),
            DecodeError::TooLong { path, limit } => Message::plain(format!(
                "\"{}\" is longer than {} hours, which is the most this application handles.",
                file_name(path),
                limit.as_secs() / 3600
            )),
            DecodeError::Empty { path } => {
                Message::plain(format!("\"{}\" contains no sound.", file_name(path)))
            }
        },
    }
}

fn resample_error(locale: Locale, error: &ResampleError) -> Message {
    match locale {
        Locale::En => match error {
            ResampleError::UnsupportedRate { rate, .. }
            | ResampleError::RateOutOfRange { rate } => Message::with_detail(
                format!(
                    "The file has a sample rate of {} Hz, which cannot be converted.",
                    rate.hertz()
                ),
                error,
            ),
            ResampleError::Process(_) | ResampleError::Buffer(_) => {
                Message::with_detail("The sound could not be prepared for the model.", error)
            }
        },
    }
}

fn inference_error(locale: Locale, error: &InferenceError) -> Message {
    match locale {
        Locale::En => match error {
            InferenceError::Load { .. } | InferenceError::MissingTensor { .. } => {
                Message::with_detail(
                    "The model could not be loaded. Its file may be damaged.",
                    error,
                )
            }
            InferenceError::GpuUnsupported => Message::plain(
                "The graphics card cannot be used on this system. Choose \"Automatic\" or \"Processor\".",
            ),
            InferenceError::GpuUnavailable { .. } => Message::with_detail(
                "The graphics card could not be used. Choose \"Automatic\" or \"Processor\".",
                error,
            ),
            InferenceError::Implausible { .. } => Message::with_detail(
                "The model produced invalid sound, so nothing was written. Try the \"Processor\" device.",
                error,
            ),
            InferenceError::Run(_)
            | InferenceError::OutputShape { .. }
            | InferenceError::MissingStem { .. }
            | InferenceError::StemNotInLayout { .. }
            | InferenceError::Stft(_) => {
                Message::with_detail("The model failed while splitting the file.", error)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, io, num::NonZeroUsize, path::PathBuf};

    use super::*;

    const EN: Locale = Locale::En;

    #[test]
    fn model_label_names_the_model_and_its_stem_count() {
        assert_eq!(
            model_label(EN, ModelId::Htdemucs),
            "htdemucs — 4 stems, fast"
        );
        assert_eq!(
            model_label(EN, ModelId::Htdemucs6s),
            "htdemucs_6s — 6 stems, adds guitar and piano"
        );
    }

    #[test]
    fn model_labels_are_distinct() {
        let labels: HashSet<String> = ModelId::ALL
            .iter()
            .map(|&model| model_label(EN, model))
            .collect();
        assert_eq!(labels.len(), ModelId::ALL.len());
    }

    #[test]
    fn megabytes_round_to_the_nearest() {
        assert_eq!(megabytes(ByteCount::ZERO), 0);
        assert_eq!(megabytes(ByteCount::new(499_999)), 0);
        assert_eq!(megabytes(ByteCount::new(500_000)), 1);
        assert_eq!(megabytes(ByteCount::new(1_499_999)), 1);
        assert_eq!(megabytes(ByteCount::new(168_683_222)), 169);
        assert_eq!(megabytes(ByteCount::new(u64::MAX)), u64::MAX / 1_000_000);
    }

    #[test]
    fn download_notice_gives_the_size() {
        assert_eq!(
            download_notice(EN, ModelId::Htdemucs6s.spec().download_size()),
            "The model (110 MB) will be downloaded first."
        );
    }

    #[test]
    fn progress_status_describes_each_stage() {
        assert_eq!(progress_status(EN, Progress::Decoding), "Reading the file…");
        assert_eq!(
            progress_status(EN, Progress::LoadingModel),
            "Loading the model…"
        );
        assert_eq!(
            progress_status(EN, Progress::Finalizing),
            "Writing the files…"
        );
        let total = NonZeroUsize::MIN.saturating_add(18);
        assert_eq!(
            progress_status(
                EN,
                Progress::Separating {
                    done: 7,
                    total,
                    device: ActiveDevice::DirectMl,
                }
            ),
            "Splitting: part 7 of 19, on the graphics card"
        );
        assert_eq!(
            progress_status(
                EN,
                Progress::Separating {
                    done: 19,
                    total,
                    device: ActiveDevice::Cpu,
                }
            ),
            "Splitting: part 19 of 19, on the processor"
        );
    }

    #[test]
    fn clock_shows_minutes_and_seconds_and_hours_when_needed() {
        assert_eq!(clock(Duration::ZERO), "0:00");
        assert_eq!(clock(Duration::from_millis(5_999)), "0:05");
        assert_eq!(clock(Duration::from_secs(65)), "1:05");
        assert_eq!(clock(Duration::from_secs(59 * 60 + 59)), "59:59");
        assert_eq!(clock(Duration::from_secs(3600 + 5)), "1:00:05");
        assert_eq!(clock(Duration::from_secs(2 * 3600 + 3 * 60 + 4)), "2:03:04");
    }

    #[test]
    fn time_status_gives_elapsed_time_and_the_estimate_when_there_is_one() {
        let elapsed = Duration::from_secs(72);

        assert_eq!(time_status(EN, elapsed, None), "Elapsed 1:12");
        assert_eq!(
            time_status(EN, elapsed, Some(Duration::from_secs(45))),
            "Elapsed 1:12 · about 0:45 left"
        );
    }

    #[test]
    fn remaining_time_is_rounded_up_and_never_shown_as_zero() {
        let elapsed = Duration::from_secs(3);

        assert_eq!(
            time_status(EN, elapsed, Some(Duration::from_millis(4_200))),
            "Elapsed 0:03 · about 0:05 left"
        );
        assert_eq!(
            time_status(EN, elapsed, Some(Duration::ZERO)),
            "Elapsed 0:03 · about 0:01 left"
        );
    }

    #[test]
    fn finished_summary_gives_the_time_taken_and_the_device() {
        assert_eq!(
            finished_summary(EN, Duration::from_millis(12_600), ActiveDevice::DirectMl),
            "Finished in 0:13 on the graphics card."
        );
        assert_eq!(
            finished_summary(EN, Duration::from_millis(55_400), ActiveDevice::Cpu),
            "Finished in 0:55 on the processor."
        );
        assert_eq!(
            finished_summary(EN, Duration::from_secs(3725), ActiveDevice::Cpu),
            "Finished in 1:02:05 on the processor."
        );
    }

    #[test]
    fn input_problem_names_the_file_not_the_whole_path() {
        let path = PathBuf::from("music").join("notes.txt");
        let message = input_problem(EN, &InputFileError::UnsupportedFormat { path });

        assert!(
            message
                .summary
                .starts_with("\"notes.txt\" is not a supported")
        );
        assert_eq!(message.detail, None);
    }

    #[test]
    fn failures_keep_the_technical_description_as_detail() {
        let error = JobError::Write(WriteError::Create {
            path: PathBuf::from("stems").join("song.vocals.wav"),
            source: io::Error::new(io::ErrorKind::PermissionDenied, "access denied"),
        });
        let message = failure(EN, &Failure::Job(error));

        assert!(
            message
                .summary
                .contains("\"song.vocals.wav\" could not be created")
        );
        let detail = message.detail.unwrap_or_default();
        assert!(detail.contains("access denied"), "{detail}");
    }

    #[test]
    fn existing_file_failure_says_nothing_was_replaced() {
        let error = JobError::Write(WriteError::AlreadyExists {
            path: PathBuf::from("song.drums.wav"),
        });

        assert_eq!(
            failure(EN, &Failure::Job(error)),
            Message::plain("\"song.drums.wav\" already exists. Nothing was replaced.")
        );
    }

    #[test]
    fn crash_and_gpu_failures_tell_the_user_what_to_do() {
        assert!(failure(EN, &Failure::Crashed).summary.contains("try again"));
        let unsupported = JobError::Inference(InferenceError::GpuUnsupported);
        assert!(
            failure(EN, &Failure::Job(unsupported))
                .summary
                .contains("\"Processor\"")
        );
    }

    #[test]
    fn device_names_are_distinct() {
        let names: HashSet<&str> = [
            DevicePreference::Auto,
            DevicePreference::Gpu,
            DevicePreference::Cpu,
        ]
        .into_iter()
        .map(|device| device_preference(EN, device))
        .collect();
        assert_eq!(names.len(), 3);
    }
}
