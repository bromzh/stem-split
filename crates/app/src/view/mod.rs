//! Rendering. Each function draws one screen from the state it is given and
//! returns what the user asked for, if anything. It keeps no state and
//! changes none; every string it shows comes from [`text`].

pub(crate) mod text;

use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use eframe::egui;
use stem_split_core::{
    domain::{ModelId, OutputChoice},
    job::{Progress, SeparationOutput},
    separate::DevicePreference,
};

use self::text::{Locale, Message, Text};
use crate::{
    form::{Form, Problem},
    worker::Failure,
};

const ROW_SPACING: f32 = 10.0;
const COLUMN_SPACING: f32 = 12.0;
const SECTION_SPACING: f32 = 12.0;
const COMBO_WIDTH: f32 = 320.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditingAction {
    PickInput,
    PickOutputFolder,
    OutputBesideInput,
    Model(ModelId),
    Device(DevicePreference),
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
    OpenFolder,
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

/// The file to split: a button to choose it, its name, and its folder.
fn file_row(ui: &mut egui::Ui, locale: Locale, form: &Form) -> Option<EditingAction> {
    let mut action = None;
    ui.label(Text::File.get(locale));
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            if ui.button(Text::Browse.get(locale)).clicked() {
                action = Some(EditingAction::PickInput);
            }
            match form.input() {
                Some(input) => ui.label(text::file_name(input.path())),
                None => ui.weak(Text::NoFileChosen.get(locale)),
            };
        });
        if let Some(input) = form.input() {
            show_folder(ui, input.directory());
        }
    });
    action
}

/// Where the stems go: next to the file, or a folder of the user's choice.
fn output_row(ui: &mut egui::Ui, locale: Locale, form: &Form) -> Option<EditingAction> {
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

/// The model, and a notice if it still has to be downloaded.
fn model_row(ui: &mut egui::Ui, locale: Locale, form: &Form) -> Option<EditingAction> {
    let mut action = None;
    let heading = ui.label(Text::Model.get(locale));
    ui.vertical(|ui| {
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
        if let Some(size) = form.download() {
            ui.weak(text::download_notice(locale, size));
        }
    });
    action
}

fn device_row(ui: &mut egui::Ui, locale: Locale, form: &Form) -> Option<EditingAction> {
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

/// The main form: what to split, where to put the result, and how.
pub(crate) fn editing(ui: &mut egui::Ui, locale: Locale, form: &Form) -> Option<EditingAction> {
    let mut action = None;
    egui::Grid::new("form")
        .num_columns(2)
        .spacing([COLUMN_SPACING, ROW_SPACING])
        .show(ui, |ui| {
            for row in [file_row, output_row, model_row, device_row] {
                // At most one widget is clicked in a frame, so a later row
                // never overwrites an earlier one's action.
                action = row(ui, locale, form).or(action);
                ui.end_row();
            }
        });

    if let Some(problem) = form.problem() {
        ui.add_space(SECTION_SPACING);
        let message = match problem {
            Problem::Input(error) => text::input_problem(locale, error),
            Problem::Output(error) => text::output_problem(locale, error),
        };
        show_message(ui, &message, ui.visuals().error_fg_color);
    }

    ui.add_space(SECTION_SPACING);
    let start = egui::Button::new(Text::Split.get(locale));
    if ui.add_enabled(form.input().is_some(), start).clicked() {
        action = Some(EditingAction::Start);
    }
    action
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
    show_file_names(ui, existing.iter().map(PathBuf::as_path));
    if let Some(folder) = existing.first().and_then(|path| path.parent()) {
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

/// How long a job has run and how long its current stage should still take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Clock {
    pub(crate) elapsed: Duration,
    pub(crate) remaining: Option<Duration>,
}

/// A job in progress, with the only thing left to decide: whether to stop it.
pub(crate) fn running(
    ui: &mut egui::Ui,
    locale: Locale,
    progress: Option<Progress>,
    clock: Clock,
    stopping: Stopping,
) -> Option<RunningAction> {
    let mut action = None;
    let status = progress.map_or_else(
        || Text::Starting.get(locale).to_owned(),
        |progress| text::progress_status(locale, progress),
    );
    ui.label(status);
    ui.add_space(ROW_SPACING);
    match progress.and_then(fraction) {
        Some(fraction) => {
            ui.add(egui::ProgressBar::new(fraction).show_percentage());
        }
        None => {
            ui.spinner();
        }
    }
    ui.weak(text::time_status(locale, clock.elapsed, clock.remaining));
    ui.add_space(SECTION_SPACING);
    let (label, enabled) = match stopping {
        Stopping::No => (Text::Cancel.get(locale), true),
        Stopping::Requested => (Text::Cancelling.get(locale), false),
    };
    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
        action = Some(RunningAction::Cancel);
    }
    action
}

/// The files a finished job produced.
pub(crate) fn done(
    ui: &mut egui::Ui,
    locale: Locale,
    folder: &Path,
    output: &SeparationOutput,
    took: Duration,
    problem: Option<&io::Error>,
) -> Option<DoneAction> {
    let mut action = None;
    ui.heading(Text::DoneHeading.get(locale));
    ui.add_space(ROW_SPACING);
    show_file_names(ui, output.files().iter().map(|(_, path)| path.as_path()));
    show_folder(ui, folder);
    ui.add_space(ROW_SPACING);
    ui.weak(text::finished_summary(locale, took, output.device()));
    if let Some(error) = problem {
        ui.add_space(ROW_SPACING);
        let message = text::open_folder_problem(locale, error);
        show_message(ui, &message, ui.visuals().error_fg_color);
    }
    ui.add_space(SECTION_SPACING);
    ui.horizontal(|ui| {
        if ui.button(Text::OpenFolder.get(locale)).clicked() {
            action = Some(DoneAction::OpenFolder);
        }
        if ui.button(Text::Back.get(locale)).clicked() {
            action = Some(DoneAction::Back);
        }
    });
    action
}

/// Why a job did not produce its files.
pub(crate) fn failed(ui: &mut egui::Ui, locale: Locale, failure: &Failure) -> Option<FailedAction> {
    let mut action = None;
    ui.heading(Text::FailedHeading.get(locale));
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
