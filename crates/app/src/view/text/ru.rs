//! Russian.

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
        Text::File => "Файл",
        Text::NoFileChosen => "Файл не выбран",
        Text::Browse => "Обзор…",
        // The two choices below continue this phrase: "Сохранить в папку с файлом".
        Text::Output => "Сохранить в",
        Text::OutputBesideFile => "Папку с файлом",
        Text::OutputOtherFolder => "Другую папку",
        Text::Model => "Модель",
        Text::Device => "Устройство",
        Text::Language => "Язык",
        Text::OpenDownloads => "Открыть папку со скачанными моделями",
        Text::ClearDownloads => "Удалить скачанные модели",
        Text::NothingDownloaded => "Модели ещё не скачивались",
        Text::Delete => "Удалить",
        Text::Split => "Разделить",
        Text::Cancel => "Отмена",
        Text::Cancelling => "Отменяется…",
        Text::Back => "Назад",
        Text::Replace => "Заменить",
        Text::OpenFolder => "Открыть папку",
        Text::AudioFiles => "Аудиофайлы",
        Text::Starting => "Запуск…",
        Text::ExistingFilesHeading => "Эти файлы уже существуют и будут заменены:",
        Text::DoneHeading => "Готово",
        Text::FailedHeading => "Файл не разделён",
        Text::NoFileSplit => "Ни один файл не разделён",
        Text::Waiting => "В очереди",
        Text::FileFailed => "Ошибка",
        Text::FileCancelled => "Отменено",
        Text::DropHint => "Отпустите аудиофайл здесь",
    }
}

/// The form of a noun that goes with `count`: `one` for 1, 21, 31…; `few`
/// for 2–4, 22–24…; `many` for everything else, including 11–14.
const fn plural(
    count: usize,
    one: &'static str,
    few: &'static str,
    many: &'static str,
) -> &'static str {
    match (count % 10, count % 100) {
        (1, tens) if tens != 11 => one,
        (2..=4, tens) if tens < 12 || tens > 14 => few,
        _ => many,
    }
}

pub(super) fn model_label(model: ModelId) -> String {
    let note = match model {
        ModelId::Htdemucs => "быстрая",
        ModelId::HtdemucsFt => "лучшее качество, примерно в 4 раза медленнее",
        ModelId::Htdemucs6s => "добавляет гитару и пианино",
    };
    let stems = model.layout().count().get();
    format!(
        "{} — {stems} {}, {note}",
        model.name(),
        plural(stems, "стем", "стема", "стемов")
    )
}

pub(super) fn download_notice(megabytes: u64) -> String {
    format!("Сначала будет скачана модель ({megabytes} МБ).")
}

pub(super) fn clear_question(megabytes: u64) -> String {
    format!(
        "Скачанные модели занимают {megabytes} МБ. Удалить их? \
         Каждая скачается заново, когда понадобится."
    )
}

pub(super) const fn device_preference(device: DevicePreference) -> &'static str {
    match device {
        DevicePreference::Auto => "Автоматически",
        DevicePreference::Gpu => "Видеокарта",
        DevicePreference::Cpu => "Процессор",
    }
}

/// The device as the place where something happens: «на …».
const fn on_device(device: ActiveDevice) -> &'static str {
    match device {
        ActiveDevice::DirectMl => "на видеокарте",
        ActiveDevice::Cpu => "на процессоре",
    }
}

pub(super) fn row_status(progress: Progress) -> String {
    match progress {
        Progress::FetchingModel(download) => format!(
            "Загрузка модели: {} из {} МБ",
            megabytes(download.downloaded()),
            megabytes(download.total())
        ),
        Progress::Decoding => "Чтение файла…".to_owned(),
        // Not «загрузка»: that word is taken by the download above.
        Progress::LoadingModel => "Подготовка модели…".to_owned(),
        Progress::Separating {
            done,
            total,
            device,
        } => format!("Часть {done} из {total} · {}", device_name(device)),
        Progress::Finalizing => "Запись файлов…".to_owned(),
    }
}

/// The device as a name: "видеокарта".
const fn device_name(device: ActiveDevice) -> &'static str {
    match device {
        ActiveDevice::DirectMl => "видеокарта",
        ActiveDevice::Cpu => "процессор",
    }
}

pub(super) fn completed_on(device: ActiveDevice) -> String {
    format!("Готово · {}", device_name(device))
}

pub(super) fn row_time(elapsed: &str, remaining: Option<&str>) -> String {
    match remaining {
        Some(remaining) => format!("{elapsed} · ещё ~{remaining}"),
        None => elapsed.to_owned(),
    }
}

pub(super) fn batch_time_status(elapsed: &str, remaining: Option<&str>) -> String {
    match remaining {
        Some(remaining) => format!("Все файлы: прошло {elapsed} · осталось около {remaining}"),
        None => format!("Все файлы: прошло {elapsed}"),
    }
}

pub(super) fn files_chosen(count: usize) -> String {
    format!("{count} {}", plural(count, "файл", "файла", "файлов"))
}

pub(super) fn partly_done(completed: usize, total: usize) -> String {
    format!("Разделено файлов: {completed} из {total}")
}

pub(super) fn total_summary(took: &str) -> String {
    format!("На все файлы ушло {took}.")
}

pub(super) fn clash_problem(first: &str, second: &str) -> Message {
    Message::plain(format!(
        "У «{first}» и «{second}» получились бы файлы с одинаковыми именами. \
         Выберите только один из них или переименуйте один."
    ))
}

pub(super) fn more_problems(count: usize) -> String {
    format!(
        "…и ещё {count} {}, которые нельзя разделить.",
        plural(count, "файл", "файла", "файлов")
    )
}

pub(super) fn finished_summary(took: &str, device: ActiveDevice) -> String {
    format!("Выполнено за {took} {}.", on_device(device))
}

pub(super) fn input_problem(error: &InputFileError) -> Message {
    match error {
        InputFileError::Inaccessible { path, .. } => {
            Message::with_detail(format!("«{}» не удалось открыть.", file_name(path)), error)
        }
        InputFileError::NotAFile { path } => {
            Message::plain(format!("«{}» — не файл.", file_name(path)))
        }
        InputFileError::UnsupportedFormat { path } => Message::plain(format!(
            "«{}» — неподдерживаемый формат. Выберите файл MP3, FLAC, WAV, OGG или M4A.",
            file_name(path)
        )),
    }
}

pub(super) fn output_problem(error: &OutputDirError) -> Message {
    match error {
        OutputDirError::Inaccessible { path, .. } => Message::with_detail(
            format!("Папку «{}» не удалось открыть.", path.display()),
            error,
        ),
        OutputDirError::NotADirectory { path } => {
            Message::plain(format!("«{}» — не папка.", path.display()))
        }
    }
}

pub(super) fn open_folder_problem(error: &io::Error) -> Message {
    Message::with_detail("Не удалось открыть папку.", error)
}

pub(super) fn downloads_problem(problem: &DownloadsProblem) -> Message {
    match problem {
        DownloadsProblem::Open(error) => open_folder_problem(error),
        DownloadsProblem::Clear(error) => match error {
            CacheError::Read { .. } => {
                Message::with_detail("Не удалось прочитать папку скачанных моделей.", error)
            }
            CacheError::Remove { path, .. } => Message::with_detail(
                format!(
                    "Не удалось удалить «{}». Возможно, он открыт в другой программе.",
                    file_name(path)
                ),
                error,
            ),
        },
        DownloadsProblem::NotStarted(error) => {
            Message::with_detail("Не удалось удалить модели.", error)
        }
        DownloadsProblem::Crashed => {
            Message::plain("Удаление моделей неожиданно прервалось. Попробуйте ещё раз.")
        }
    }
}

pub(super) fn start_failure(error: &eframe::Error) -> Message {
    Message::with_detail(
        "Не удалось запустить приложение. Возможно, поможет обновление драйвера видеокарты.",
        error,
    )
}

pub(super) fn failure(failure: &Failure) -> Message {
    match failure {
        Failure::Job(error) => job_error(error),
        Failure::NotStarted(error) => Message::with_detail("Не удалось начать работу.", error),
        Failure::Crashed => {
            Message::plain("Работа неожиданно прервалась. Ничего не записано. Попробуйте ещё раз.")
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
            "Файл «{}» уже существует. Ничего не заменено.",
            file_name(path)
        )),
        WriteError::Create { path, .. } => Message::with_detail(
            format!(
                "Не удалось создать «{}». Проверьте, что в папку можно записывать.",
                file_name(path)
            ),
            error,
        ),
        WriteError::Write { path, .. } => Message::with_detail(
            format!(
                "Не удалось записать «{}». Проверьте, что на диске есть место.",
                file_name(path)
            ),
            error,
        ),
        WriteError::Publish { path, .. } => Message::with_detail(
            format!(
                "Не удалось поместить «{}» на место. Возможно, файл открыт в другой программе.",
                file_name(path)
            ),
            error,
        ),
        WriteError::TooLong { path } => Message::plain(format!(
            "«{}» получился бы больше, чем допускает формат WAV.",
            file_name(path)
        )),
    }
}

fn model_error(error: &ModelStoreError) -> Message {
    match error {
        ModelStoreError::NoCacheDirectory => {
            Message::plain("Для этой учётной записи нет папки, где можно хранить модель.")
        }
        ModelStoreError::CreateDirectory { path, .. }
        | ModelStoreError::Write { path, .. }
        | ModelStoreError::Publish { path, .. } => Message::with_detail(
            format!(
                "Не удалось сохранить модель в «{}». Проверьте, что на диске есть место.",
                path.display()
            ),
            error,
        ),
        ModelStoreError::Request { .. } => Message::with_detail(
            "Не удалось скачать модель. Проверьте подключение к интернету и попробуйте ещё раз.",
            error,
        ),
        ModelStoreError::Transfer { .. } => {
            Message::with_detail("Загрузка модели прервалась. Попробуйте ещё раз.", error)
        }
        ModelStoreError::WrongSize { .. } | ModelStoreError::DigestMismatch { .. } => {
            Message::with_detail(
                "Скачанная модель повреждена и удалена. Попробуйте ещё раз.",
                error,
            )
        }
    }
}

fn decode_error(error: &DecodeError) -> Message {
    match error {
        DecodeError::Open { path, .. } => {
            Message::with_detail(format!("Не удалось открыть «{}».", file_name(path)), error)
        }
        DecodeError::Unrecognized { path, .. } | DecodeError::NoAudioTrack { path } => {
            Message::with_detail(
                format!(
                    "«{}» — не аудио, которое умеет читать приложение.",
                    file_name(path)
                ),
                error,
            )
        }
        DecodeError::UnsupportedCodec { path, .. } => Message::with_detail(
            format!(
                "«{}» записан в аудиоформате, который приложение не читает.",
                file_name(path)
            ),
            error,
        ),
        DecodeError::Read { path, .. }
        | DecodeError::InvalidSampleRate { path }
        | DecodeError::FormatChanged { path } => Message::with_detail(
            format!("«{}» повреждён и не читается.", file_name(path)),
            error,
        ),
        // «Каналов: N» sidesteps the plural form of the noun.
        DecodeError::UnsupportedChannels { path, channels } => Message::plain(format!(
            "В «{}» каналов: {channels}. Разделять можно только моно и стерео.",
            file_name(path)
        )),
        DecodeError::TooLong { path, limit } => Message::plain(format!(
            "«{}» длиннее {} ч — это предел для приложения.",
            file_name(path),
            limit.as_secs() / 3600
        )),
        DecodeError::Empty { path } => {
            Message::plain(format!("В «{}» нет звука.", file_name(path)))
        }
    }
}

fn resample_error(error: &ResampleError) -> Message {
    match error {
        ResampleError::UnsupportedRate { rate, .. } | ResampleError::RateOutOfRange { rate } => {
            Message::with_detail(
                format!(
                    "Частота дискретизации файла — {} Гц, её не удаётся преобразовать.",
                    rate.hertz()
                ),
                error,
            )
        }
        ResampleError::Process(_) | ResampleError::Buffer(_) => {
            Message::with_detail("Не удалось подготовить звук для модели.", error)
        }
    }
}

fn inference_error(error: &InferenceError) -> Message {
    match error {
        InferenceError::Load { .. } | InferenceError::MissingTensor { .. } => Message::with_detail(
            "Не удалось загрузить модель. Возможно, её файл повреждён.",
            error,
        ),
        InferenceError::GpuUnsupported => Message::plain(
            "На этой системе видеокарту использовать нельзя. Выберите «Автоматически» или «Процессор».",
        ),
        InferenceError::GpuUnavailable { .. } => Message::with_detail(
            "Не удалось использовать видеокарту. Выберите «Автоматически» или «Процессор».",
            error,
        ),
        InferenceError::Implausible { .. } => Message::with_detail(
            "Модель выдала некорректный звук, поэтому ничего не записано. Попробуйте устройство «Процессор».",
            error,
        ),
        InferenceError::Run(_)
        | InferenceError::OutputShape { .. }
        | InferenceError::MissingStem { .. }
        | InferenceError::StemNotInLayout { .. }
        | InferenceError::Stft(_) => {
            Message::with_detail("Модель дала сбой при разделении файла.", error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plural_follows_russian_number_agreement() {
        let stems = |count| plural(count, "стем", "стема", "стемов");

        assert_eq!(stems(1), "стем");
        assert_eq!(stems(2), "стема");
        assert_eq!(stems(4), "стема");
        assert_eq!(stems(5), "стемов");
        assert_eq!(stems(6), "стемов");
        assert_eq!(stems(11), "стемов");
        assert_eq!(stems(12), "стемов");
        assert_eq!(stems(14), "стемов");
        assert_eq!(stems(21), "стем");
        assert_eq!(stems(22), "стема");
        assert_eq!(stems(25), "стемов");
        assert_eq!(stems(111), "стемов");
        assert_eq!(stems(0), "стемов");
    }
}
