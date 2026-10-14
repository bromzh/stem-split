//! English.

use std::io;

use stem_split_core::{
    audio::{DecodeError, ResampleError, WriteError},
    domain::{InputFileError, ModelId, OutputDirError},
    job::{JobError, Progress},
    model::{CacheError, ModelStoreError},
    separate::{ActiveDevice, DevicePreference, InferenceError},
};

use super::{Message, Text, file_name, megabytes};
use crate::{downloads::DownloadsProblem, worker::Failure};

pub(super) const fn text(text: Text) -> &'static str {
    match text {
        Text::WindowTitle => "stem-split",
        Text::File => "File",
        Text::NoFileChosen => "No file chosen",
        Text::Browse => "Browse…",
        Text::Output => "Save to",
        Text::OutputBesideFile => "The folder of the file",
        Text::OutputOtherFolder => "Another folder",
        Text::Model => "Model",
        Text::Device => "Device",
        Text::Language => "Language",
        Text::OpenDownloads => "Open the folder of downloaded models",
        Text::ClearDownloads => "Delete downloaded models",
        Text::NothingDownloaded => "No models have been downloaded yet",
        Text::Delete => "Delete",
        Text::Split => "Split",
        Text::Cancel => "Cancel",
        Text::Cancelling => "Cancelling…",
        Text::Back => "Back",
        Text::Replace => "Replace",
        Text::OpenFolder => "Open folder",
        Text::AudioFiles => "Audio files",
        Text::Starting => "Starting…",
        Text::ExistingFilesHeading => "These files already exist and will be replaced:",
        Text::DoneHeading => "Done",
        Text::FailedHeading => "The file was not split",
        Text::NoFileSplit => "No file was split",
        Text::Waiting => "Waiting",
        Text::FileFailed => "Failed",
        Text::FileCancelled => "Cancelled",
        Text::DropHint => "Drop the audio file here",
    }
}

pub(super) fn model_label(model: ModelId) -> String {
    let note = match model {
        ModelId::Htdemucs => "fast",
        ModelId::HtdemucsFt => "best quality, about 4x slower",
        ModelId::Htdemucs6s => "adds guitar and piano",
    };
    format!(
        "{} — {} stems, {note}",
        model.name(),
        model.layout().count()
    )
}

pub(super) fn download_notice(megabytes: u64) -> String {
    format!("The model ({megabytes} MB) will be downloaded first.")
}

pub(super) fn clear_question(megabytes: u64) -> String {
    format!(
        "The downloaded models take {megabytes} MB. Delete them? \
         Each one will be downloaded again the next time it is used."
    )
}

pub(super) const fn device_preference(device: DevicePreference) -> &'static str {
    match device {
        DevicePreference::Auto => "Automatic",
        DevicePreference::Gpu => "Graphics card",
        DevicePreference::Cpu => "Processor",
    }
}

/// The device as the place where something happens: "on the …".
const fn on_device(device: ActiveDevice) -> &'static str {
    match device {
        ActiveDevice::DirectMl => "on the graphics card",
        ActiveDevice::Cpu => "on the processor",
    }
}

pub(super) fn row_status(progress: Progress) -> String {
    match progress {
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
        } => format!("Part {done} of {total} · {}", device_name(device)),
        Progress::Finalizing => "Writing the files…".to_owned(),
    }
}

/// The device as a name: "graphics card".
const fn device_name(device: ActiveDevice) -> &'static str {
    match device {
        ActiveDevice::DirectMl => "graphics card",
        ActiveDevice::Cpu => "processor",
    }
}

pub(super) fn completed_on(device: ActiveDevice) -> String {
    format!("Done · {}", device_name(device))
}

pub(super) fn row_time(elapsed: &str, remaining: Option<&str>) -> String {
    match remaining {
        Some(remaining) => format!("{elapsed} · ~{remaining} left"),
        None => elapsed.to_owned(),
    }
}

pub(super) fn batch_time_status(elapsed: &str, remaining: Option<&str>) -> String {
    match remaining {
        Some(remaining) => format!("All files: elapsed {elapsed} · about {remaining} left"),
        None => format!("All files: elapsed {elapsed}"),
    }
}

pub(super) fn files_chosen(count: usize) -> String {
    format!("{count} files")
}

pub(super) fn partly_done(completed: usize, total: usize) -> String {
    format!("{completed} of {total} files split")
}

pub(super) fn total_summary(took: &str) -> String {
    format!("All files took {took}.")
}

pub(super) fn clash_problem(first: &str, second: &str) -> Message {
    Message::plain(format!(
        "\"{first}\" and \"{second}\" would be split into files with the same names. \
         Choose only one of them, or rename one."
    ))
}

pub(super) fn more_problems(count: usize) -> String {
    let files = if count == 1 { "file" } else { "files" };
    format!("…and {count} more {files} that cannot be split.")
}

pub(super) fn finished_summary(took: &str, device: ActiveDevice) -> String {
    format!("Finished in {took} {}.", on_device(device))
}

pub(super) fn input_problem(error: &InputFileError) -> Message {
    match error {
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
    }
}

pub(super) fn output_problem(error: &OutputDirError) -> Message {
    match error {
        OutputDirError::Inaccessible { path, .. } => Message::with_detail(
            format!("The folder \"{}\" cannot be opened.", path.display()),
            error,
        ),
        OutputDirError::NotADirectory { path } => {
            Message::plain(format!("\"{}\" is not a folder.", path.display()))
        }
    }
}

pub(super) fn open_folder_problem(error: &io::Error) -> Message {
    Message::with_detail("The folder could not be opened.", error)
}

pub(super) fn downloads_problem(problem: &DownloadsProblem) -> Message {
    match problem {
        DownloadsProblem::Open(error) => open_folder_problem(error),
        DownloadsProblem::Clear(error) => match error {
            CacheError::Read { .. } => {
                Message::with_detail("The folder of downloaded models cannot be read.", error)
            }
            CacheError::Remove { path, .. } => Message::with_detail(
                format!(
                    "\"{}\" could not be deleted. It may be in use by another program.",
                    file_name(path)
                ),
                error,
            ),
        },
        DownloadsProblem::NotStarted(error) => {
            Message::with_detail("The models could not be deleted.", error)
        }
        DownloadsProblem::Crashed => {
            Message::plain("Deleting the models stopped unexpectedly. Please try again.")
        }
    }
}

pub(super) fn start_failure(error: &eframe::Error) -> Message {
    Message::with_detail(
        "The application could not start. Updating the graphics driver may help.",
        error,
    )
}

pub(super) fn failure(failure: &Failure) -> Message {
    match failure {
        Failure::Job(error) => job_error(error),
        Failure::NotStarted(error) => Message::with_detail("The work could not be started.", error),
        Failure::Crashed => {
            Message::plain("The work stopped unexpectedly. Nothing was written. Please try again.")
        }
    }
}

fn job_error(error: &JobError) -> Message {
    match error {
        JobError::Write(error) => write_error(error),
        JobError::Model(error) => model_error(error),
        JobError::Decode(error) => decode_error(error),
        JobError::Resample(error) => resample_error(error),
        JobError::Inference(error) => inference_error(error),
    }
}

fn write_error(error: &WriteError) -> Message {
    match error {
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
    }
}

fn model_error(error: &ModelStoreError) -> Message {
    match error {
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
    }
}

fn decode_error(error: &DecodeError) -> Message {
    match error {
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
    }
}

fn resample_error(error: &ResampleError) -> Message {
    match error {
        ResampleError::UnsupportedRate { rate, .. } | ResampleError::RateOutOfRange { rate } => {
            Message::with_detail(
                format!(
                    "The file has a sample rate of {} Hz, which cannot be converted.",
                    rate.hertz()
                ),
                error,
            )
        }
        ResampleError::Process(_) | ResampleError::Buffer(_) => {
            Message::with_detail("The sound could not be prepared for the model.", error)
        }
    }
}

fn inference_error(error: &InferenceError) -> Message {
    match error {
        InferenceError::Load { .. } | InferenceError::MissingTensor { .. } => Message::with_detail(
            "The model could not be loaded. Its file may be damaged.",
            error,
        ),
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
    }
}
