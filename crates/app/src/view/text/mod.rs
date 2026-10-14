//! Every user-visible string, for every interface language.
//!
//! Each language is a module with the same set of functions. The functions
//! here pick the module by exhaustive `match` on [`Locale`], and the modules
//! pick the string by exhaustive `match` on what is being described. Adding a
//! language, a [`Text`] or an error variant in the core therefore does not
//! compile until every string exists in every language.

mod en;
mod ru;

use std::{io, path::Path, time::Duration};

use stem_split_core::{
    closed_enum,
    domain::{ByteCount, InputFileError, ModelId, OutputDirError},
    job::Progress,
    separate::{ActiveDevice, DevicePreference},
};

use crate::{downloads::DownloadsProblem, worker::Failure};

/// Decimal megabytes, the unit download sizes are usually quoted in.
const BYTES_PER_MEGABYTE: u64 = 1_000_000;

closed_enum! {
    /// Interface language.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) enum Locale {
        #[default]
        En,
        Ru,
    }
}

impl Locale {
    /// The language's name in that language. It is shown in the language
    /// list as is, whatever the current language, so that it can be found by
    /// someone who cannot read the interface.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Ru => "Русский",
        }
    }

    /// The ISO 639-1 code of the language; identifies it in saved settings.
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ru => "ru",
        }
    }

    pub(crate) fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|locale| locale.code() == code)
    }

    /// The language of a tag such as `ru-RU` or `en_US`, if it is one of ours.
    pub(crate) fn from_language_tag(tag: &str) -> Option<Self> {
        let language = tag.split(['-', '_']).next()?;
        Self::ALL
            .iter()
            .copied()
            .find(|locale| locale.code().eq_ignore_ascii_case(language))
    }

    /// The language the operating system is set to, or the default if it is
    /// not one of ours.
    pub(crate) fn of_system() -> Self {
        sys_locale::get_locale()
            .and_then(|tag| Self::from_language_tag(&tag))
            .unwrap_or_default()
    }
}

closed_enum! {
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
        Language,
        OpenDownloads,
        ClearDownloads,
        NothingDownloaded,
        Delete,
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
        DropHint,
    }
}

impl Text {
    pub(crate) const fn get(self, locale: Locale) -> &'static str {
        match locale {
            Locale::En => en::text(self),
            Locale::Ru => ru::text(self),
        }
    }
}

/// Megabytes, rounded to the nearest whole one.
fn megabytes(bytes: ByteCount) -> u64 {
    bytes.get().saturating_add(BYTES_PER_MEGABYTE / 2) / BYTES_PER_MEGABYTE
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

/// Rounded to the nearest whole second: the way to state a final figure,
/// where 12.6 s is "13 seconds" and not "12".
fn to_the_nearest_second(duration: Duration) -> Duration {
    Duration::from_secs(
        duration
            .saturating_add(Duration::from_millis(500))
            .as_secs(),
    )
}

/// The last component of `path`, the part a user recognizes a file by.
pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// A message for the user: what happened in plain words, and the technical
/// description for a bug report. The description comes from the error itself
/// and is not translated.
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

/// One line describing a model, such as `htdemucs — 4 stems, fast`.
pub(crate) fn model_label(locale: Locale, model: ModelId) -> String {
    match locale {
        Locale::En => en::model_label(model),
        Locale::Ru => ru::model_label(model),
    }
}

/// Tells the user that starting will first download the model.
pub(crate) fn download_notice(locale: Locale, size: ByteCount) -> String {
    let megabytes = megabytes(size);
    match locale {
        Locale::En => en::download_notice(megabytes),
        Locale::Ru => ru::download_notice(megabytes),
    }
}

/// Asks before the downloaded models are deleted, and says how much room
/// that frees.
pub(crate) fn clear_question(locale: Locale, size: ByteCount) -> String {
    let megabytes = megabytes(size);
    match locale {
        Locale::En => en::clear_question(megabytes),
        Locale::Ru => ru::clear_question(megabytes),
    }
}

pub(crate) const fn device_preference(locale: Locale, device: DevicePreference) -> &'static str {
    match locale {
        Locale::En => en::device_preference(device),
        Locale::Ru => ru::device_preference(device),
    }
}

/// What a running job is doing, in one line.
pub(crate) fn progress_status(locale: Locale, progress: Progress) -> String {
    match locale {
        Locale::En => en::progress_status(progress),
        Locale::Ru => ru::progress_status(progress),
    }
}

/// The time a job has taken so far and, when it can be estimated, the time
/// its current stage still needs.
pub(crate) fn time_status(
    locale: Locale,
    elapsed: Duration,
    remaining: Option<Duration>,
) -> String {
    let elapsed = clock(elapsed);
    let remaining = remaining.map(|remaining| clock(at_least_a_second(remaining)));
    match locale {
        Locale::En => en::time_status(&elapsed, remaining.as_deref()),
        Locale::Ru => ru::time_status(&elapsed, remaining.as_deref()),
    }
}

/// The line under the list of results: how long the job took, from the
/// moment it was started to the last file being in place, and where it
/// was computed.
pub(crate) fn finished_summary(locale: Locale, took: Duration, device: ActiveDevice) -> String {
    let took = clock(to_the_nearest_second(took));
    match locale {
        Locale::En => en::finished_summary(&took, device),
        Locale::Ru => ru::finished_summary(&took, device),
    }
}

/// Why a file the user chose cannot be split.
pub(crate) fn input_problem(locale: Locale, error: &InputFileError) -> Message {
    match locale {
        Locale::En => en::input_problem(error),
        Locale::Ru => ru::input_problem(error),
    }
}

/// Why a folder the user chose cannot receive the results.
pub(crate) fn output_problem(locale: Locale, error: &OutputDirError) -> Message {
    match locale {
        Locale::En => en::output_problem(error),
        Locale::Ru => ru::output_problem(error),
    }
}

/// Why the results folder could not be shown.
pub(crate) fn open_folder_problem(locale: Locale, error: &io::Error) -> Message {
    match locale {
        Locale::En => en::open_folder_problem(error),
        Locale::Ru => ru::open_folder_problem(error),
    }
}

/// Why the folder of downloaded models could not be shown or emptied.
pub(crate) fn downloads_problem(locale: Locale, problem: &DownloadsProblem) -> Message {
    match locale {
        Locale::En => en::downloads_problem(problem),
        Locale::Ru => ru::downloads_problem(problem),
    }
}

/// Why no window appeared at all.
pub(crate) fn start_failure(locale: Locale, error: &eframe::Error) -> Message {
    match locale {
        Locale::En => en::start_failure(error),
        Locale::Ru => ru::start_failure(error),
    }
}

/// Why a job did not produce its files.
pub(crate) fn failure(locale: Locale, failure: &Failure) -> Message {
    match locale {
        Locale::En => en::failure(failure),
        Locale::Ru => ru::failure(failure),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, num::NonZeroUsize, path::PathBuf};

    use stem_split_core::{
        audio::WriteError,
        job::JobError,
        model::{CacheError, DownloadProgress, ModelStoreError},
        separate::InferenceError,
    };

    use super::*;

    const EN: Locale = Locale::En;
    const RU: Locale = Locale::Ru;

    #[test]
    fn every_text_exists_in_every_language() {
        for &locale in Locale::ALL {
            for &text in Text::ALL {
                assert!(!text.get(locale).is_empty(), "{text:?} in {locale:?}");
            }
        }
    }

    #[test]
    fn languages_do_not_share_translations_by_accident() {
        // Only the application name is the same everywhere.
        for &text in Text::ALL {
            let same = text.get(EN) == text.get(RU);
            assert_eq!(same, text == Text::WindowTitle, "{text:?}");
        }
    }

    #[test]
    fn locale_codes_round_trip_and_are_distinct() {
        let codes: HashSet<&str> = Locale::ALL.iter().map(|locale| locale.code()).collect();
        assert_eq!(codes.len(), Locale::ALL.len());
        for &locale in Locale::ALL {
            assert_eq!(Locale::from_code(locale.code()), Some(locale));
            assert!(!locale.name().is_empty());
        }
        assert_eq!(Locale::from_code("de"), None);
        assert_eq!(Locale::from_code(""), None);
    }

    #[test]
    fn system_language_tags_are_matched_by_their_language_part() {
        assert_eq!(Locale::from_language_tag("ru-RU"), Some(RU));
        assert_eq!(Locale::from_language_tag("ru"), Some(RU));
        assert_eq!(Locale::from_language_tag("RU_ru"), Some(RU));
        assert_eq!(Locale::from_language_tag("en-US"), Some(EN));
        assert_eq!(Locale::from_language_tag("en_GB.UTF-8"), Some(EN));
        assert_eq!(Locale::from_language_tag("de-DE"), None);
        assert_eq!(Locale::from_language_tag(""), None);
        // A language that merely starts with the same letters is another one.
        assert_eq!(Locale::from_language_tag("rue"), None);
    }

    #[test]
    fn model_labels_name_the_model_and_its_stem_count() {
        assert_eq!(
            model_label(EN, ModelId::Htdemucs),
            "htdemucs — 4 stems, fast"
        );
        assert_eq!(
            model_label(EN, ModelId::Htdemucs6s),
            "htdemucs_6s — 6 stems, adds guitar and piano"
        );
        assert_eq!(
            model_label(RU, ModelId::Htdemucs),
            "htdemucs — 4 стема, быстрая"
        );
        assert_eq!(
            model_label(RU, ModelId::Htdemucs6s),
            "htdemucs_6s — 6 стемов, добавляет гитару и пианино"
        );
    }

    #[test]
    fn model_labels_are_distinct_in_every_language() {
        for &locale in Locale::ALL {
            let labels: HashSet<String> = ModelId::ALL
                .iter()
                .map(|&model| model_label(locale, model))
                .collect();
            assert_eq!(labels.len(), ModelId::ALL.len(), "{locale:?}");
        }
    }

    #[test]
    fn device_names_are_distinct_in_every_language() {
        for &locale in Locale::ALL {
            let names: HashSet<&str> = DevicePreference::ALL
                .iter()
                .map(|&device| device_preference(locale, device))
                .collect();
            assert_eq!(names.len(), DevicePreference::ALL.len(), "{locale:?}");
        }
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
        let size = ModelId::Htdemucs6s.spec().download_size();
        assert_eq!(
            download_notice(EN, size),
            "The model (110 MB) will be downloaded first."
        );
        assert_eq!(
            download_notice(RU, size),
            "Сначала будет скачана модель (110 МБ)."
        );
    }

    #[test]
    fn clear_question_gives_the_size() {
        let size = ByteCount::new(954_386_000);
        assert_eq!(
            clear_question(EN, size),
            "The downloaded models take 954 MB. Delete them? \
             Each one will be downloaded again the next time it is used."
        );
        assert_eq!(
            clear_question(RU, size),
            "Скачанные модели занимают 954 МБ. Удалить их? \
             Каждая скачается заново, когда понадобится."
        );
    }

    #[test]
    fn file_that_cannot_be_deleted_is_named_with_the_likely_reason() {
        let problem = || {
            DownloadsProblem::Clear(CacheError::Remove {
                path: PathBuf::from("models").join("htdemucs-abaac48bc8fd55fa.onnx"),
                source: io::Error::new(io::ErrorKind::PermissionDenied, "access denied"),
            })
        };

        let english = downloads_problem(EN, &problem());
        assert_eq!(
            english.summary,
            "\"htdemucs-abaac48bc8fd55fa.onnx\" could not be deleted. \
             It may be in use by another program."
        );
        let russian = downloads_problem(RU, &problem());
        assert_eq!(
            russian.summary,
            "Не удалось удалить «htdemucs-abaac48bc8fd55fa.onnx». \
             Возможно, он открыт в другой программе."
        );
        for message in [english, russian] {
            let detail = message.detail.unwrap_or_default();
            assert!(detail.contains("access denied"), "{detail}");
        }
    }

    #[test]
    fn progress_status_describes_each_stage() {
        let total = NonZeroUsize::MIN.saturating_add(18);
        let separating = |done, device| Progress::Separating {
            done,
            total,
            device,
        };
        let downloading = Progress::FetchingModel(DownloadProgress::new(
            ByteCount::new(45_000_000),
            ByteCount::new(168_683_222),
        ));

        assert_eq!(progress_status(EN, Progress::Decoding), "Reading the file…");
        assert_eq!(
            progress_status(EN, Progress::LoadingModel),
            "Loading the model…"
        );
        assert_eq!(
            progress_status(EN, Progress::Finalizing),
            "Writing the files…"
        );
        assert_eq!(
            progress_status(EN, downloading),
            "Downloading the model: 45 of 169 MB"
        );
        assert_eq!(
            progress_status(EN, separating(7, ActiveDevice::DirectMl)),
            "Splitting: part 7 of 19, on the graphics card"
        );
        assert_eq!(
            progress_status(EN, separating(19, ActiveDevice::Cpu)),
            "Splitting: part 19 of 19, on the processor"
        );

        assert_eq!(progress_status(RU, Progress::Decoding), "Чтение файла…");
        assert_eq!(
            progress_status(RU, Progress::LoadingModel),
            "Подготовка модели…"
        );
        assert_eq!(progress_status(RU, Progress::Finalizing), "Запись файлов…");
        assert_eq!(
            progress_status(RU, downloading),
            "Загрузка модели: 45 из 169 МБ"
        );
        assert_eq!(
            progress_status(RU, separating(7, ActiveDevice::DirectMl)),
            "Разделение: часть 7 из 19, на видеокарте"
        );
        assert_eq!(
            progress_status(RU, separating(19, ActiveDevice::Cpu)),
            "Разделение: часть 19 из 19, на процессоре"
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
        let remaining = Some(Duration::from_secs(45));

        assert_eq!(time_status(EN, elapsed, None), "Elapsed 1:12");
        assert_eq!(
            time_status(EN, elapsed, remaining),
            "Elapsed 1:12 · about 0:45 left"
        );
        assert_eq!(time_status(RU, elapsed, None), "Прошло 1:12");
        assert_eq!(
            time_status(RU, elapsed, remaining),
            "Прошло 1:12 · осталось около 0:45"
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
        assert_eq!(
            finished_summary(RU, Duration::from_millis(12_600), ActiveDevice::DirectMl),
            "Выполнено за 0:13 на видеокарте."
        );
        assert_eq!(
            finished_summary(RU, Duration::from_millis(55_400), ActiveDevice::Cpu),
            "Выполнено за 0:55 на процессоре."
        );
    }

    #[test]
    fn input_problem_names_the_file_not_the_whole_path() {
        let path = PathBuf::from("music").join("notes.txt");
        let error = InputFileError::UnsupportedFormat { path };

        let english = input_problem(EN, &error);
        assert!(
            english
                .summary
                .starts_with("\"notes.txt\" is not a supported")
        );
        assert_eq!(english.detail, None);

        let russian = input_problem(RU, &error);
        assert!(
            russian
                .summary
                .starts_with("«notes.txt» — неподдерживаемый")
        );
        assert_eq!(russian.detail, None);
    }

    #[test]
    fn failures_keep_the_technical_description_as_detail() {
        let error = || {
            Failure::Job(JobError::Write(WriteError::Create {
                path: PathBuf::from("stems").join("song.vocals.wav"),
                source: io::Error::new(io::ErrorKind::PermissionDenied, "access denied"),
            }))
        };

        let english = failure(EN, &error());
        assert!(
            english
                .summary
                .contains("\"song.vocals.wav\" could not be created")
        );
        let russian = failure(RU, &error());
        assert!(
            russian
                .summary
                .contains("Не удалось создать «song.vocals.wav»")
        );
        // The description comes from the error and is the same in both.
        for message in [english, russian] {
            let detail = message.detail.unwrap_or_default();
            assert!(detail.contains("access denied"), "{detail}");
        }
    }

    #[test]
    fn existing_file_failure_says_nothing_was_replaced() {
        let error = || {
            Failure::Job(JobError::Write(WriteError::AlreadyExists {
                path: PathBuf::from("song.drums.wav"),
            }))
        };

        assert_eq!(
            failure(EN, &error()),
            Message::plain("\"song.drums.wav\" already exists. Nothing was replaced.")
        );
        assert_eq!(
            failure(RU, &error()),
            Message::plain("Файл «song.drums.wav» уже существует. Ничего не заменено.")
        );
    }

    #[test]
    fn failures_tell_the_user_what_to_do() {
        let unsupported = || Failure::Job(JobError::Inference(InferenceError::GpuUnsupported));
        let no_cache = || Failure::Job(JobError::Model(ModelStoreError::NoCacheDirectory));

        assert!(failure(EN, &Failure::Crashed).summary.contains("try again"));
        assert!(
            failure(RU, &Failure::Crashed)
                .summary
                .contains("Попробуйте ещё раз")
        );
        // The advice names the device the way the device list does.
        for &locale in Locale::ALL {
            let advice = failure(locale, &unsupported()).summary;
            let processor = device_preference(locale, DevicePreference::Cpu);
            assert!(advice.contains(processor), "{locale:?}: {advice}");
            assert!(!failure(locale, &no_cache()).summary.is_empty());
        }
    }
}
