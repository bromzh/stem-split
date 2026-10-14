//! Rendering. Each function draws one screen from the state it is given and
//! returns what the user asked for, if anything. It keeps no state and
//! changes none; every string it shows comes from [`text`].

pub(crate) mod text;

use std::{
    io,
    path::{Path, PathBuf},
};

use eframe::egui;
use stem_split_core::{
    domain::{ByteCount, ModelId, OutputChoice},
    job::Progress,
    model::Usage,
    separate::DevicePreference,
};

use self::text::{Locale, Message, Text};
use crate::{
    downloads::Downloads,
    form::{Form, Problem},
    tracker::{Outcome, Overview, Results, RowStatus},
    worker::Failure,
};

const ROW_SPACING: f32 = 10.0;
const COLUMN_SPACING: f32 = 12.0;
const SECTION_SPACING: f32 = 12.0;
const COMBO_WIDTH: f32 = 320.0;
/// How dark the window gets while a file is dragged over it: 0 is not at
/// all, 255 is black.
const DROP_SHADE: u8 = 190;
const DROP_HINT_SIZE: f32 = 22.0;
/// The picture on the language button. The bundled emoji fonts have it.
const GLOBE: &str = "\u{1F310}";
const GLOBE_SIZE: f32 = 16.0;
/// The pictures on the buttons for downloaded models, from the same bundled
/// icon font so that they match.
const OPEN_FOLDER: &str = "\u{1F5C1}";
const WASTEBASKET: &str = "\u{1F5D1}";
/// How wide the question before deleting the models may grow.
const QUESTION_WIDTH: f32 = 260.0;
/// How many names of chosen files show before the list scrolls. The half
/// line shows that there is more.
const VISIBLE_INPUTS: f32 = 4.5;
/// How many of the files that cannot be split are named one by one.
const LISTED_PROBLEMS: usize = 3;
/// Columns of a row on the progress screen: the file name and the time,
/// with the bar taking the width between them.
const NAME_WIDTH: f32 = 170.0;
const TIME_WIDTH: f32 = 120.0;
const MIN_BAR_WIDTH: f32 = 120.0;
/// Room kept under a list of files for what follows it.
const FOOTER_HEIGHT: f32 = 80.0;
/// A list of files is never squeezed below this height.
const MIN_LIST_HEIGHT: f32 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditingAction {
    PickInputs,
    PickOutputFolder,
    OutputBesideInput,
    Model(ModelId),
    Device(DevicePreference),
    Language(Locale),
    OpenDownloads,
    ClearDownloads,
    Start,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmingAction {
    Replace,
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunningAction {
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DoneAction {
    /// The folder of the results of this file, by its position.
    OpenFolder(usize),
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailedAction {
    Back,
}

/// Whether the user has already asked a running job to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stopping {
    No,
    Requested,
}

fn show_message(ui: &mut egui::Ui, message: &Message, color: egui::Color32) {
    ui.colored_label(color, &message.summary);
    if let Some(detail) = &message.detail {
        ui.add(egui::Label::new(egui::RichText::new(detail).small().weak()).selectable(true));
    }
}

/// A folder path on one line, cut short with an ellipsis when the window is
/// too narrow for it. The whole path is in the tooltip.
fn show_folder(ui: &mut egui::Ui, folder: &Path) {
    let path = folder.display().to_string();
    ui.add(egui::Label::new(egui::RichText::new(&path).weak()).truncate())
        .on_hover_text(path);
}

/// The names of `files`, one per line.
fn show_file_names<'a>(ui: &mut egui::Ui, files: impl Iterator<Item = &'a Path>) {
    for path in files {
        ui.monospace(text::file_name(path));
    }
}

/// Gives a button that shows only a picture a name in words. A picture says
/// nothing to a screen reader; the caller shows the words in a tooltip too.
fn name_picture(response: egui::Response, name: &str) -> egui::Response {
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, response.enabled(), name)
    });
    response
}

/// The files to split: a button to choose them, and their names. A single
/// file also shows its folder; several are listed, scrolling if need be.
fn file_row(
    ui: &mut egui::Ui,
    locale: Locale,
    form: &Form,
    _downloads: Option<&Downloads>,
) -> Option<EditingAction> {
    let mut action = None;
    ui.label(Text::File.get(locale));
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            if ui.button(Text::Browse.get(locale)).clicked() {
                action = Some(EditingAction::PickInputs);
            }
            match form.inputs() {
                None => ui.weak(Text::NoFileChosen.get(locale)),
                Some(inputs) if inputs.count().get() == 1 => {
                    ui.label(text::file_name(inputs.first().path()))
                }
                Some(inputs) => ui.label(text::files_chosen(locale, inputs.count())),
            };
        });
        match form.inputs() {
            None => {}
            Some(inputs) if inputs.count().get() == 1 => {
                show_folder(ui, inputs.first().directory());
            }
            Some(inputs) => {
                // A line of text is as high as a button, so that the two line
                // up in a row.
                let line = ui.spacing().interact_size.y;
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a count of files shown on screen is small"
                )]
                let lines = (inputs.count().get() as f32).min(VISIBLE_INPUTS);
                // The cell of a grid offers only the height its row had in
                // the previous frame; without a minimum the list would
                // never grow past its first size.
                egui::ScrollArea::vertical()
                    .id_salt("inputs")
                    .min_scrolled_height(line * lines)
                    .max_height(line * lines)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for input in inputs.iter() {
                            let name = egui::RichText::new(text::file_name(input.path())).weak();
                            ui.add(egui::Label::new(name).truncate())
                                .on_hover_text(input.path().display().to_string());
                        }
                    });
            }
        }
    });
    action
}

/// Where the stems go: next to the file, or a folder of the user's choice.
fn output_row(
    ui: &mut egui::Ui,
    locale: Locale,
    form: &Form,
    _downloads: Option<&Downloads>,
) -> Option<EditingAction> {
    let mut action = None;
    ui.label(Text::Output.get(locale));
    ui.vertical(|ui| {
        let custom = match form.output() {
            OutputChoice::BesideInput => None,
            OutputChoice::Custom(directory) => Some(directory),
        };
        if ui
            .radio(custom.is_none(), Text::OutputBesideFile.get(locale))
            .clicked()
        {
            action = Some(EditingAction::OutputBesideInput);
        }
        ui.horizontal(|ui| {
            if ui
                .radio(custom.is_some(), Text::OutputOtherFolder.get(locale))
                .clicked()
            {
                action = Some(EditingAction::PickOutputFolder);
            }
            if custom.is_some() && ui.button(Text::Browse.get(locale)).clicked() {
                action = Some(EditingAction::PickOutputFolder);
            }
        });
        if let Some(directory) = custom {
            show_folder(ui, directory.path());
        }
    });
    action
}

/// The model, the buttons for downloaded models, and a notice if the model
/// still has to be downloaded.
fn model_row(
    ui: &mut egui::Ui,
    locale: Locale,
    form: &Form,
    downloads: Option<&Downloads>,
) -> Option<EditingAction> {
    let mut action = None;
    let heading = ui.label(Text::Model.get(locale));
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("model")
                .width(COMBO_WIDTH)
                .selected_text(text::model_label(locale, form.model()))
                .show_ui(ui, |ui| {
                    for &model in ModelId::ALL {
                        let label = text::model_label(locale, model);
                        if ui.selectable_label(model == form.model(), label).clicked() {
                            action = Some(EditingAction::Model(model));
                        }
                    }
                })
                .response
                // Names the list for screen readers, which otherwise announce
                // only its current value.
                .labelled_by(heading.id);
            if let Some(downloads) = downloads {
                action = downloads_buttons(ui, locale, downloads).or(action);
            }
        });
        if let Some(size) = form.download() {
            ui.weak(text::download_notice(locale, size));
        }
        if let Some(problem) = downloads.and_then(Downloads::problem) {
            let message = text::downloads_problem(locale, problem);
            show_message(ui, &message, ui.visuals().error_fg_color);
        }
    });
    action
}

/// Two small buttons after the model list: one shows the folder the models
/// are downloaded to, the other deletes them after asking.
fn downloads_buttons(
    ui: &mut egui::Ui,
    locale: Locale,
    downloads: &Downloads,
) -> Option<EditingAction> {
    let mut action = None;
    let usage = downloads.usage();
    let folder = downloads.directory().display();

    // A folder that does not exist yet has nothing to show. Its path is in
    // the tooltip all the same: that is where the models will go.
    let exists = usage != Some(Usage::NoDirectory);
    let open = ui.add_enabled(exists, egui::Button::new(OPEN_FOLDER));
    if open.clicked() {
        action = Some(EditingAction::OpenDownloads);
    }
    name_picture(open, Text::OpenDownloads.get(locale))
        .on_hover_text(format!("{}\n{folder}", Text::OpenDownloads.get(locale)))
        .on_disabled_hover_text(format!("{}\n{folder}", Text::NothingDownloaded.get(locale)));

    let stored = match usage {
        Some(Usage::Files(size)) if size != ByteCount::ZERO => Some(size),
        Some(Usage::Files(_) | Usage::NoDirectory) | None => None,
    };
    // Deleting asks first, in a small window that opens from the button and
    // closes at a click anywhere else.
    let clear = ui
        .add_enabled_ui(stored.is_some(), |ui| {
            let button = egui::Button::new(WASTEBASKET);
            let (response, _) =
                egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
                    let Some(size) = stored else { return };
                    ui.set_max_width(QUESTION_WIDTH);
                    ui.add(egui::Label::new(text::clear_question(locale, size)).wrap());
                    // Coloured as a warning: this one cannot be undone.
                    let delete = egui::RichText::new(Text::Delete.get(locale))
                        .color(ui.visuals().error_fg_color);
                    if ui.button(delete).clicked() {
                        action = Some(EditingAction::ClearDownloads);
                        ui.close();
                    }
                });
            response
        })
        .inner;
    let unavailable = match usage {
        Some(Usage::Files(_) | Usage::NoDirectory) => Text::NothingDownloaded,
        // Being deleted right now, or the folder cannot be read.
        None => Text::ClearDownloads,
    };
    name_picture(clear, Text::ClearDownloads.get(locale))
        .on_hover_text(Text::ClearDownloads.get(locale))
        .on_disabled_hover_text(unavailable.get(locale));
    action
}

fn device_row(
    ui: &mut egui::Ui,
    locale: Locale,
    form: &Form,
    _downloads: Option<&Downloads>,
) -> Option<EditingAction> {
    let mut action = None;
    let heading = ui.label(Text::Device.get(locale));
    egui::ComboBox::from_id_salt("device")
        .width(COMBO_WIDTH)
        .selected_text(text::device_preference(locale, form.device()))
        .show_ui(ui, |ui| {
            for &device in DevicePreference::ALL {
                let label = text::device_preference(locale, device);
                if ui
                    .selectable_label(device == form.device(), label)
                    .clicked()
                {
                    action = Some(EditingAction::Device(device));
                }
            }
        })
        .response
        .labelled_by(heading.id);
    action
}

/// The interface language: a faint globe in the corner that opens the list
/// of languages, each under its own name.
///
/// It is kept out of the way on purpose. A language is chosen once, and the
/// form should not spend a row on it; a picture instead of a word can be
/// found by someone who cannot read the current language.
fn language_menu(ui: &mut egui::Ui, locale: Locale) -> Option<EditingAction> {
    let mut action = None;
    let globe = egui::RichText::new(GLOBE).size(GLOBE_SIZE).weak();
    let button = egui::Button::new(globe).frame(false);
    let (response, _) = egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
        for &language in Locale::ALL {
            if ui
                .selectable_label(language == locale, language.name())
                .clicked()
            {
                action = Some(EditingAction::Language(language));
                ui.close();
            }
        }
    });
    let name = Text::Language.get(locale);
    name_picture(response, name).on_hover_text(name);
    action
}

/// Covers the window while a file is dragged over it, to show that it can
/// be dropped.
pub(crate) fn drop_hint(context: &egui::Context, locale: Locale) {
    let area = context.content_rect();
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop hint"));
    let painter = context.layer_painter(layer);
    painter.rect_filled(area, 0.0, egui::Color32::from_black_alpha(DROP_SHADE));
    painter.text(
        area.center(),
        egui::Align2::CENTER_CENTER,
        Text::DropHint.get(locale),
        egui::FontId::proportional(DROP_HINT_SIZE),
        egui::Color32::WHITE,
    );
}

/// The main form: what to split, where to put the result, and how.
/// `downloads` is absent when there is no model store to show.
pub(crate) fn editing(
    ui: &mut egui::Ui,
    locale: Locale,
    form: &Form,
    downloads: Option<&Downloads>,
) -> Option<EditingAction> {
    let mut action = None;
    egui::Grid::new("form")
        .num_columns(2)
        .spacing([COLUMN_SPACING, ROW_SPACING])
        .show(ui, |ui| {
            for row in [file_row, output_row, model_row, device_row] {
                // At most one widget is clicked in a frame, so a later row
                // never overwrites an earlier one's action.
                action = row(ui, locale, form, downloads).or(action);
                ui.end_row();
            }
        });

    if let Some(problem) = form.problem() {
        ui.add_space(SECTION_SPACING);
        show_problem(ui, locale, problem);
    }

    ui.add_space(SECTION_SPACING);
    let start = egui::Button::new(Text::Split.get(locale));
    // A job must not start while the models it needs are being deleted.
    let clearing = downloads.is_some_and(Downloads::is_clearing);
    if ui
        .add_enabled(form.inputs().is_some() && !clearing, start)
        .clicked()
    {
        action = Some(EditingAction::Start);
    }
    // Whatever height is left goes to a layout that fills from the bottom
    // and aligns to the right: its only widget lands in that corner.
    let corner = egui::Layout::bottom_up(egui::Align::Max);
    ui.with_layout(corner, |ui| language_menu(ui, locale))
        .inner
        .or(action)
}

/// Why something on the form was not accepted. Of many files that cannot
/// be split only the first few are named.
fn show_problem(ui: &mut egui::Ui, locale: Locale, problem: &Problem) {
    let color = ui.visuals().error_fg_color;
    match problem {
        Problem::Inputs(errors) => {
            for error in errors.iter().take(LISTED_PROBLEMS) {
                show_message(ui, &text::input_problem(locale, error), color);
            }
            let unnamed = errors.len().saturating_sub(LISTED_PROBLEMS);
            if unnamed > 0 {
                ui.colored_label(color, text::more_problems(locale, unnamed));
            }
        }
        Problem::Output(error) => show_message(ui, &text::output_problem(locale, error), color),
        Problem::Clash(clash) => show_message(ui, &text::clash_problem(locale, clash), color),
    }
}

/// The folder all of `paths` are in, if they are in one.
fn common_parent(paths: &[PathBuf]) -> Option<&Path> {
    let mut parents = paths.iter().map(|path| path.parent());
    let first = parents.next()??;
    parents.all(|parent| parent == Some(first)).then_some(first)
}

/// How high a list may grow and still leave room for what follows it.
fn list_height(ui: &egui::Ui) -> f32 {
    (ui.available_height() - FOOTER_HEIGHT).max(MIN_LIST_HEIGHT)
}

/// Asks before replacing results of an earlier run.
pub(crate) fn confirming(
    ui: &mut egui::Ui,
    locale: Locale,
    existing: &[PathBuf],
) -> Option<ConfirmingAction> {
    let mut action = None;
    ui.label(Text::ExistingFilesHeading.get(locale));
    ui.add_space(ROW_SPACING);
    egui::ScrollArea::vertical()
        .max_height(list_height(ui))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            show_file_names(ui, existing.iter().map(PathBuf::as_path));
        });
    if let Some(folder) = common_parent(existing) {
        show_folder(ui, folder);
    }
    ui.add_space(SECTION_SPACING);
    ui.horizontal(|ui| {
        if ui.button(Text::Replace.get(locale)).clicked() {
            action = Some(ConfirmingAction::Replace);
        }
        if ui.button(Text::Back.get(locale)).clicked() {
            action = Some(ConfirmingAction::Back);
        }
    });
    action
}

/// How far `progress` is through its stage, where that can be told.
#[expect(
    clippy::cast_precision_loss,
    reason = "a progress bar does not need more than f32 precision"
)]
fn fraction(progress: Progress) -> Option<f32> {
    match progress {
        Progress::FetchingModel(download) => {
            let total = download.total().get().max(1);
            Some(download.downloaded().get() as f32 / total as f32)
        }
        Progress::Separating { done, total, .. } => Some(done as f32 / total.get() as f32),
        Progress::Finalizing => Some(1.0),
        Progress::Decoding | Progress::LoadingModel => None,
    }
}

/// What the bar of a row shows.
struct Bar {
    /// How full it is; `None` for work whose progress cannot be told, which
    /// is shown as moving.
    fraction: Option<f32>,
    text: String,
    /// Colours the bar of a file that failed.
    fill: Option<egui::Color32>,
}

impl Bar {
    fn of_progress(locale: Locale, progress: Progress) -> Self {
        Self {
            fraction: fraction(progress),
            text: text::row_status(locale, progress),
            fill: None,
        }
    }

    fn of_row(ui: &egui::Ui, locale: Locale, status: RowStatus) -> Self {
        let still = |fraction, text: Text| Self {
            fraction: Some(fraction),
            text: text.get(locale).to_owned(),
            fill: None,
        };
        match status {
            RowStatus::Waiting => still(0.0, Text::Waiting),
            RowStatus::Working(None) => Self {
                fraction: None,
                text: Text::Starting.get(locale).to_owned(),
                fill: None,
            },
            RowStatus::Working(Some(progress)) => Self::of_progress(locale, progress),
            RowStatus::Completed(device) => Self {
                fraction: Some(1.0),
                text: text::completed_on(locale, device),
                fill: None,
            },
            RowStatus::Failed => Self {
                fill: Some(ui.visuals().error_fg_color),
                ..still(1.0, Text::FileFailed)
            },
            RowStatus::Cancelled => still(0.0, Text::FileCancelled),
        }
    }
}

/// One line of the progress screen: a name, a bar telling what is going on,
/// and the time next to the bar.
fn progress_line(ui: &mut egui::Ui, name: &str, hint: &str, bar: Bar, time: &str) {
    ui.horizontal(|ui| {
        let size = egui::vec2(NAME_WIDTH, ui.spacing().interact_size.y);
        ui.allocate_ui_with_layout(
            size,
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(NAME_WIDTH);
                ui.add(egui::Label::new(name).truncate())
                    .on_hover_text(hint);
            },
        );
        let spacing = ui.spacing().item_spacing.x;
        let width = (ui.available_width() - TIME_WIDTH - spacing).max(MIN_BAR_WIDTH);
        let mut progress = egui::ProgressBar::new(bar.fraction.unwrap_or(0.0))
            .desired_width(width)
            .animate(bar.fraction.is_none())
            .text(bar.text);
        if let Some(fill) = bar.fill {
            progress = progress.fill(fill);
        }
        ui.add(progress);
        ui.weak(time);
    });
}

/// A batch in progress: a row for each file, the time of the whole batch
/// under them when there is more than one, and the button to stop.
pub(crate) fn running(
    ui: &mut egui::Ui,
    locale: Locale,
    overview: &Overview<'_>,
    stopping: Stopping,
) -> Option<RunningAction> {
    let mut action = None;
    egui::ScrollArea::vertical()
        .max_height(list_height(ui))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if let Some(download) = overview.download {
                let progress = Progress::FetchingModel(download.progress);
                let time = text::row_time(locale, download.clock);
                let name = download.model.name();
                progress_line(ui, name, name, Bar::of_progress(locale, progress), &time);
            }
            for row in &overview.rows {
                let time = row
                    .clock
                    .map(|clock| text::row_time(locale, clock))
                    .unwrap_or_default();
                progress_line(
                    ui,
                    &text::file_name(row.input),
                    &row.input.display().to_string(),
                    Bar::of_row(ui, locale, row.status),
                    &time,
                );
            }
        });
    ui.add_space(SECTION_SPACING);
    if overview.rows.len() > 1 {
        ui.label(text::batch_time_status(locale, overview.total));
        ui.add_space(ROW_SPACING);
    }
    let (label, enabled) = match stopping {
        Stopping::No => (Text::Cancel.get(locale), true),
        Stopping::Requested => (Text::Cancelling.get(locale), false),
    };
    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
        action = Some(RunningAction::Cancel);
    }
    action
}

/// What became of every file of a batch. When the results went to more than
/// one folder, each file has a button for its own.
pub(crate) fn done(
    ui: &mut egui::Ui,
    locale: Locale,
    results: &Results,
    problem: Option<&io::Error>,
) -> Option<DoneAction> {
    let mut action = None;
    let completed = results.completed();
    ui.heading(text::results_heading(
        locale,
        completed,
        results.files.len(),
    ));
    ui.add_space(ROW_SPACING);
    let common = results.common_folder();
    egui::ScrollArea::vertical()
        .max_height(list_height(ui))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (index, file) in results.files.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.strong(text::file_name(&file.input))
                        .on_hover_text(file.input.display().to_string());
                    let completed = matches!(file.outcome, Outcome::Completed { .. });
                    if common.is_none() && completed {
                        let open = ui.small_button(OPEN_FOLDER);
                        if open.clicked() {
                            action = Some(DoneAction::OpenFolder(index));
                        }
                        let name = Text::OpenFolder.get(locale);
                        name_picture(open, name)
                            .on_hover_text(format!("{name}\n{}", file.folder.display()));
                    }
                });
                match &file.outcome {
                    Outcome::Completed { output, took } => {
                        ui.weak(text::finished_summary(locale, *took, output.device()));
                        let names: Vec<String> = output
                            .files()
                            .iter()
                            .map(|(_, path)| text::file_name(path))
                            .collect();
                        let names = egui::RichText::new(names.join(", ")).small().weak();
                        ui.add(egui::Label::new(names).wrap());
                    }
                    Outcome::Failed(failure) => show_message(
                        ui,
                        &text::failure(locale, failure),
                        ui.visuals().error_fg_color,
                    ),
                    Outcome::Cancelled => {
                        ui.weak(Text::FileCancelled.get(locale));
                    }
                }
                ui.add_space(ROW_SPACING);
            }
        });
    if results.files.len() > 1 {
        ui.label(text::total_summary(locale, results.took));
    }
    if let Some(folder) = common {
        show_folder(ui, folder);
    }
    if let Some(error) = problem {
        ui.add_space(ROW_SPACING);
        let message = text::open_folder_problem(locale, error);
        show_message(ui, &message, ui.visuals().error_fg_color);
    }
    ui.add_space(SECTION_SPACING);
    ui.horizontal(|ui| {
        if common.is_some() && completed > 0 && ui.button(Text::OpenFolder.get(locale)).clicked() {
            action = Some(DoneAction::OpenFolder(0));
        }
        if ui.button(Text::Back.get(locale)).clicked() {
            action = Some(DoneAction::Back);
        }
    });
    action
}

/// Why a batch could not run at all, for instance because its model could
/// not be downloaded. `files` is how many files it had.
pub(crate) fn failed(
    ui: &mut egui::Ui,
    locale: Locale,
    files: usize,
    failure: &Failure,
) -> Option<FailedAction> {
    let mut action = None;
    ui.heading(text::results_heading(locale, 0, files));
    ui.add_space(ROW_SPACING);
    show_message(
        ui,
        &text::failure(locale, failure),
        ui.visuals().text_color(),
    );
    ui.add_space(SECTION_SPACING);
    if ui.button(Text::Back.get(locale)).clicked() {
        action = Some(FailedAction::Back);
    }
    action
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use stem_split_core::separate::ActiveDevice;

    use super::*;

    #[test]
    fn stages_without_a_measure_have_no_fraction() {
        assert_eq!(fraction(Progress::Decoding), None);
        assert_eq!(fraction(Progress::LoadingModel), None);
    }

    #[test]
    fn separation_fraction_is_runs_done_over_total() {
        let total = NonZeroUsize::MIN.saturating_add(3);
        let progress = |done| Progress::Separating {
            done,
            total,
            device: ActiveDevice::Cpu,
        };

        assert_eq!(fraction(progress(0)), Some(0.0));
        assert_eq!(fraction(progress(1)), Some(0.25));
        assert_eq!(fraction(progress(4)), Some(1.0));
        assert_eq!(fraction(Progress::Finalizing), Some(1.0));
    }
}
