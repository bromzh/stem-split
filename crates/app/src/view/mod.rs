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
    domain::{ByteCount, EnhancerId, ModelId, OutputChoice, Stem, StemSet},
    job::Progress,
    model::Usage,
    separate::DevicePreference,
};

use self::text::{Locale, Message, Text};
use crate::{
    downloads::Downloads,
    form::{Form, Mode, Problem, Source, StartBlock},
    settings::SeparationPanel,
    tracker::{Outcome, Overview, Results, RowStatus},
    worker::Failure,
};

const ROW_SPACING: f32 = 10.0;
const COLUMN_SPACING: f32 = 12.0;
const SECTION_SPACING: f32 = 12.0;
const COMBO_WIDTH: f32 = 320.0;
/// Equal cells keep the common settings still when changing input tabs.
const OUTPUT_HEIGHT: f32 = 60.0;
/// Reserved before metadata arrives, so a long playlist never resizes the window.
const METADATA_HEIGHT: f32 = 24.0;
const PLAYLIST_WIDTH: f32 = 300.0;
const PLAYLIST_GAP: f32 = 12.0;
const FORM_LABEL_WIDTH: f32 = 112.0;
const FORM_CONTENT_WIDTH: f32 = 624.0;
pub(crate) const INITIAL_WINDOW_SIZE: [f32; 2] = [640.0, 540.0];
const CLEAR_INPUT: &str = "\u{00D7}";
const DISCLOSURE_CLOSED: &str = "\u{25B6}";
const DISCLOSURE_OPEN: &str = "\u{25BC}";
const DOWNLOAD_WIDTH: f32 = 96.0;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EditingAction {
    Settings,
    SeparationPanel(SeparationPanel),
    PlaylistPanel(crate::playlist::Panel),
    PlaylistItem(usize),
    PlaylistSelection(crate::playlist::Selection),
    Source(Source),
    Url(String),
    PickInputs,
    PickOutputFolder,
    OutputBesideInput,
    Model(ModelId),
    Device(DevicePreference),
    ToggleStem(Stem),
    AllStems,
    NoStems,
    KeepStems(bool),
    Cleanup(bool),
    CleanupStems(bool),
    CleanupMix(bool),
    Enhancer(EnhancerId),
    Language(Locale),
    OpenDownloads,
    ClearDownloads,
    Start(Mode),
    Download,
    SaveFormat(stem_split_media::SaveFormat),
    OutputFormat(stem_split_core::domain::OutputFormat),
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
    input_cell(ui, source_height(ui, form), |ui| {
        ui.horizontal(|ui| {
            if ui.button(Text::Browse.get(locale)).clicked() {
                action = Some(EditingAction::PickInputs);
            }
            match form.inputs() {
                None => ui.weak(Text::NoFileChosen.get(locale)),
                Some(inputs) if inputs.count().get() == 1 => {
                    ui.add(egui::Label::new(text::file_name(inputs.first().path())).truncate())
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

fn input_cell<R>(
    ui: &mut egui::Ui,
    height: f32,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let width = FORM_CONTENT_WIDTH - FORM_LABEL_WIDTH - COLUMN_SPACING;
    let size = egui::vec2(width, height);
    ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_width(width);
        ui.set_min_height(height);
        contents(ui)
    })
}

#[expect(
    clippy::cast_precision_loss,
    reason = "the visible file count is capped at a few rows"
)]
fn source_height(ui: &egui::Ui, form: &Form) -> f32 {
    // Keep tabs aligned using the actual local selection, without reserving
    // a full file list when the form is empty or contains just one file.
    let lines =
        if form.source() == Source::Url && form.separation_panel() == SeparationPanel::Collapsed {
            0.0
        } else {
            form.local_inputs().map_or(0.0, |inputs| {
                if inputs.count().get() == 1 {
                    1.0
                } else {
                    (inputs.count().get() as f32).min(VISIBLE_INPUTS)
                }
            })
        };
    let spacing = ui.spacing().item_spacing.y;
    let control = ui
        .spacing()
        .interact_size
        .y
        .max(ui.text_style_height(&egui::TextStyle::Body) + 2.0 * ui.spacing().button_padding.y)
        .ceil();
    let hint = 2.0 * ui.text_style_height(&egui::TextStyle::Body);
    (control + hint + 3.0 * spacing)
        .max(control + METADATA_HEIGHT + spacing + 2.0)
        .max(control * (1.0 + lines) + 2.0 * spacing)
        .ceil()
}

fn source_row(
    ui: &mut egui::Ui,
    locale: Locale,
    form: &Form,
    downloads: Option<&Downloads>,
) -> Option<EditingAction> {
    match form.source() {
        Source::Files => file_row(ui, locale, form, downloads),
        Source::Url => {
            let heading = ui.label(Text::Url.get(locale));
            input_cell(ui, source_height(ui, form), |ui| {
                let mut url = form.url().to_owned();
                let mut action = None;
                let blocked = start_block(form, downloads, Mode::Split);
                ui.horizontal(|ui| {
                    let button_size = ui.spacing().interact_size.y;
                    let width = (ui.available_width()
                        - button_size
                        - DOWNLOAD_WIDTH
                        - 2.0 * ui.spacing().item_spacing.x)
                        .max(80.0);
                    let response = ui
                        .add_sized(
                            [width, button_size],
                            egui::TextEdit::singleline(&mut url)
                                .hint_text(Text::UrlPlaceholder.get(locale)),
                        )
                        .labelled_by(heading.id);
                    if response.changed() {
                        action = Some(EditingAction::Url(url.clone()));
                    }
                    let clear = ui.add_enabled(
                        !url.is_empty(),
                        egui::Button::new(CLEAR_INPUT)
                            .min_size(egui::vec2(button_size, button_size)),
                    );
                    if clear.clicked() {
                        action = Some(EditingAction::Url(String::new()));
                        response.request_focus();
                    }
                    name_picture(clear, Text::ClearUrl.get(locale))
                        .on_hover_text(Text::ClearUrl.get(locale));
                    let download = ui.add_enabled(
                        blocked.is_none(),
                        egui::Button::new(Text::Download.get(locale))
                            .min_size(egui::vec2(DOWNLOAD_WIDTH, button_size)),
                    );
                    if download.clicked() {
                        action = Some(EditingAction::Download);
                    }
                    if let Some(reason) = blocked {
                        download.on_disabled_hover_text(text::start_block(locale, reason));
                    }
                });
                action = url_preview(ui, locale, form).or(action);
                action
            })
            .inner
        }
    }
}

/// Reserved title and playlist-toggle space that cannot grow the form.
fn url_preview(ui: &mut egui::Ui, locale: Locale, form: &Form) -> Option<EditingAction> {
    let mut action = None;
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), METADATA_HEIGHT),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_min_height(METADATA_HEIGHT);
            match form.preview() {
                crate::preview::State::Empty | crate::preview::State::Pending { .. } => {
                    ui.add(
                        egui::Label::new(egui::RichText::new(Text::UrlHint.get(locale)).weak())
                            .truncate(),
                    );
                }
                crate::preview::State::Loading(_) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.weak(Text::LoadingMetadata.get(locale));
                    });
                }
                crate::preview::State::Ready(info) => {
                    action = media_title(
                        ui,
                        locale,
                        info,
                        form.playlist().map(|(_, choices)| choices.panel()),
                    );
                }
                crate::preview::State::Failed(error) => {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(Text::MetadataFailed.get(locale))
                                .color(ui.visuals().error_fg_color),
                        )
                        .truncate(),
                    )
                    .on_hover_text({
                        let message = text::failure(locale, error);
                        message.detail.unwrap_or(message.summary)
                    });
                }
            }
        },
    );
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
    if form.source() == Source::Url {
        input_cell(ui, OUTPUT_HEIGHT, |ui| {
            ui.horizontal(|ui| {
                if ui.button(Text::Browse.get(locale)).clicked() {
                    action = Some(EditingAction::PickOutputFolder);
                }
                let mut format = form.save_format();
                egui::ComboBox::from_id_salt("save encoding")
                    .width(110.0)
                    .selected_text(text::save_format(locale, format))
                    .show_ui(ui, |ui| {
                        for &choice in stem_split_media::SaveFormat::ALL {
                            ui.selectable_value(
                                &mut format,
                                choice,
                                text::save_format(locale, choice),
                            );
                        }
                    })
                    .response
                    .on_hover_text(Text::SaveFormat.get(locale));
                if format != form.save_format() {
                    action = Some(EditingAction::SaveFormat(format));
                }
            });
            match form.url_folder() {
                None => {
                    ui.weak(Text::RequiredFolder.get(locale));
                }
                Some(folder) => show_folder(ui, folder.path()),
            }
        });
        return action;
    }
    input_cell(ui, OUTPUT_HEIGHT, |ui| {
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
                action =
                    downloads_buttons(ui, locale, downloads, form.preview_busy()).or(action.take());
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
    preview_busy: bool,
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
        .add_enabled_ui(stored.is_some() && !preview_busy, |ui| {
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
        .on_disabled_hover_text(if preview_busy {
            Text::LoadingMetadata.get(locale)
        } else {
            unavailable.get(locale)
        });
    action
}

/// The stems of the current model to put in a mix, ticks for all or none of
/// them, and whether the stems are saved next to the mix.
fn stems_row(
    ui: &mut egui::Ui,
    locale: Locale,
    form: &Form,
    _downloads: Option<&Downloads>,
) -> Option<EditingAction> {
    let mut action = None;
    ui.label(Text::Stems.get(locale));
    ui.vertical(|ui| {
        let layout = form.model().layout();
        let selected = form.mix_stems();
        ui.horizontal_wrapped(|ui| {
            for stem in layout.iter() {
                let mut ticked = selected.contains(stem);
                if ui
                    .checkbox(&mut ticked, text::stem_label(locale, stem))
                    .clicked()
                {
                    action = Some(EditingAction::ToggleStem(stem));
                }
            }
        });
        ui.horizontal(|ui| {
            // "All" ticked again unticks everything, as a master checkbox
            // does; "None" only ever clears.
            let mut all = selected == StemSet::all(layout);
            if ui.checkbox(&mut all, Text::AllStems.get(locale)).clicked() {
                action = Some(if all {
                    EditingAction::AllStems
                } else {
                    EditingAction::NoStems
                });
            }
            let mut none = selected.is_empty();
            if ui.checkbox(&mut none, Text::NoStems.get(locale)).clicked() {
                action = Some(EditingAction::NoStems);
            }
            ui.separator();
            let mut keep = form.keeps_stems();
            if ui
                .checkbox(&mut keep, Text::KeepStems.get(locale))
                .on_hover_text(Text::KeepStemsHint.get(locale))
                .clicked()
            {
                action = Some(EditingAction::KeepStems(keep));
            }
        });
    });
    action
}

/// Whether the results are cleaned up, whether the stems, the mix or both,
/// and by which network.
fn cleanup_row(
    ui: &mut egui::Ui,
    locale: Locale,
    form: &Form,
    _downloads: Option<&Downloads>,
) -> Option<EditingAction> {
    let mut action = None;
    let heading = ui.label(Text::Cleanup.get(locale));
    ui.vertical(|ui| {
        let cleanup = form.cleanup();
        let mut enabled = cleanup.enabled;
        if ui
            .checkbox(&mut enabled, Text::CleanUpArtifacts.get(locale))
            .clicked()
        {
            action = Some(EditingAction::Cleanup(enabled));
        }
        ui.add_enabled_ui(cleanup.enabled, |ui| {
            ui.horizontal(|ui| {
                let mut stems = cleanup.stems;
                if ui
                    .checkbox(&mut stems, Text::CleanupStems.get(locale))
                    .on_hover_text(Text::CleanupStemsHint.get(locale))
                    .clicked()
                {
                    action = Some(EditingAction::CleanupStems(stems));
                }
                // A network that tells vocals from instruments cannot clean a
                // mix of both; the tick stays as it was, but counts for nothing.
                let possible = form.can_clean_mix();
                let mut mix = cleanup.mix && possible;
                let response = ui
                    .add_enabled(
                        possible,
                        egui::Checkbox::new(&mut mix, Text::CleanupMix.get(locale)),
                    )
                    .on_hover_text(Text::CleanupMixHint.get(locale))
                    .on_disabled_hover_text(Text::CleanupMixUnavailable.get(locale));
                if response.clicked() {
                    action = Some(EditingAction::CleanupMix(mix));
                }
            });
            // A list of one would offer no choice; its only network is
            // named instead.
            if EnhancerId::ALL.len() > 1 {
                egui::ComboBox::from_id_salt("enhancer")
                    .width(COMBO_WIDTH)
                    // A label wider than the list is cut short instead of
                    // pushing the list past the edge of the window.
                    .wrap_mode(egui::TextWrapMode::Truncate)
                    .selected_text(text::enhancer_label(locale, cleanup.enhancer))
                    .show_ui(ui, |ui| {
                        for &enhancer in EnhancerId::ALL {
                            let label = text::enhancer_label(locale, enhancer);
                            if ui
                                .selectable_label(enhancer == cleanup.enhancer, label)
                                .clicked()
                            {
                                action = Some(EditingAction::Enhancer(enhancer));
                            }
                        }
                    })
                    .response
                    .on_hover_text(Text::CleanupNetwork.get(locale))
                    .labelled_by(heading.id);
            } else {
                ui.weak(text::enhancer_label(locale, cleanup.enhancer))
                    .on_hover_text(Text::CleanupNetwork.get(locale));
            }
        });
    });
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
#[derive(Debug)]
pub(crate) struct EditingView {
    pub(crate) action: Option<EditingAction>,
    pub(crate) minimum: egui::Vec2,
}

pub(crate) fn editing(
    ui: &mut egui::Ui,
    locale: Locale,
    form: &Form,
    downloads: Option<&Downloads>,
) -> EditingView {
    let padding = (ui.ctx().content_rect().size() - ui.available_size()).max(egui::Vec2::ZERO);
    let playlist = form.playlist().filter(|(_, choices)| {
        form.source() == Source::Url && choices.panel() == crate::playlist::Panel::Open
    });
    let extra_width = if playlist.is_some() {
        PLAYLIST_WIDTH + PLAYLIST_GAP
    } else {
        0.0
    };
    let available = ui.available_rect_before_wrap();
    let mut main = available;
    main.max.x = (main.max.x - extra_width).max(main.min.x + 1.0);
    // Only the main form determines minimum height. The side list is clipped
    // to the current viewport and scrolls independently, even for large lists.
    let content = ui
        .scope_builder(egui::UiBuilder::new().max_rect(main), |ui| {
            egui::ScrollArea::both()
                .id_salt("small screen form")
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_width(FORM_CONTENT_WIDTH);
                    editing_contents(ui, locale, form, downloads)
                })
        })
        .inner;
    let mut action = content.inner;
    if let Some((info, choices)) = playlist {
        let panel = egui::Rect::from_min_max(
            egui::pos2(main.max.x + PLAYLIST_GAP, available.min.y),
            available.max,
        );
        let divider = main.max.x + PLAYLIST_GAP / 2.0;
        ui.painter().line_segment(
            [
                egui::pos2(divider, available.min.y),
                egui::pos2(divider, available.max.y),
            ],
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
        let selected = ui
            .scope_builder(egui::UiBuilder::new().max_rect(panel), |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(panel));
                playlist_panel(ui, locale, info, choices)
            })
            .inner;
        action = selected.or(action);
    }
    EditingView {
        action,
        minimum: (content.content_size + padding + egui::vec2(extra_width, 4.0)).ceil(),
    }
}

fn media_title(
    ui: &mut egui::Ui,
    locale: Locale,
    info: &stem_split_media::MediaInfo,
    toggle: Option<crate::playlist::Panel>,
) -> Option<EditingAction> {
    let mut action = None;
    let count = match info {
        stem_split_media::MediaInfo::Single(_) => None,
        stem_split_media::MediaInfo::Playlist { .. } => Some(format!(
            "({})",
            text::playlist_count(locale, info.items().count())
        )),
    };
    ui.horizontal(|ui| {
        let width = |value: &str| {
            ui.painter()
                .layout_no_wrap(
                    value.to_owned(),
                    egui::TextStyle::Body.resolve(ui.style()),
                    ui.visuals().text_color(),
                )
                .size()
                .x
        };
        let spacing = ui.spacing().item_spacing.x;
        let count_width = count.as_ref().map_or(0.0, |count| width(count) + spacing);
        let toggle_width = if toggle.is_some() {
            width(Text::Playlist.get(locale)) + 2.0 * ui.spacing().button_padding.x + spacing
        } else {
            0.0
        };
        let title_width =
            width(info.title()).min((ui.available_width() - count_width - toggle_width).max(1.0));
        ui.add_sized(
            [title_width, ui.text_style_height(&egui::TextStyle::Body)],
            egui::Label::new(info.title()).truncate(),
        )
        .on_hover_text(info.title());
        if let Some(count) = count {
            ui.weak(count);
        }
        if let Some(panel) = toggle {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(Text::Playlist.get(locale))
                            .selected(panel == crate::playlist::Panel::Open),
                    )
                    .clicked()
                {
                    let next = match panel {
                        crate::playlist::Panel::Open => crate::playlist::Panel::Closed,
                        crate::playlist::Panel::Closed => crate::playlist::Panel::Open,
                    };
                    action = Some(EditingAction::PlaylistPanel(next));
                }
            });
        }
    });
    action
}

fn playlist_panel(
    ui: &mut egui::Ui,
    locale: Locale,
    info: &stem_split_media::MediaInfo,
    choices: &crate::playlist::Choices,
) -> Option<EditingAction> {
    let mut action = None;
    ui.set_width(PLAYLIST_WIDTH);
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
    ui.strong(Text::Playlist.get(locale));
    media_title(ui, locale, info, None);
    let hint = if info
        .items()
        .enumerate()
        .any(|(index, _)| choices.includes(index))
    {
        Text::PlaylistSelectionHint
    } else {
        Text::NoPlaylistItems
    };
    ui.add(egui::Label::new(egui::RichText::new(hint.get(locale)).weak()).wrap());
    ui.horizontal(|ui| {
        use crate::playlist::Selection;
        let mut all = info
            .items()
            .enumerate()
            .all(|(index, _)| choices.includes(index));
        let mut none = info
            .items()
            .enumerate()
            .all(|(index, _)| !choices.includes(index));
        if ui
            .checkbox(&mut all, Text::PlaylistAll.get(locale))
            .clicked()
        {
            action = Some(EditingAction::PlaylistSelection(if all {
                Selection::All
            } else {
                Selection::None
            }));
        }
        if ui
            .checkbox(&mut none, Text::PlaylistNone.get(locale))
            .clicked()
        {
            action = Some(EditingAction::PlaylistSelection(if none {
                Selection::None
            } else {
                Selection::All
            }));
        }
    });
    let selected = info
        .items()
        .enumerate()
        .filter(|(index, _)| choices.includes(*index))
        .count();
    ui.weak(text::playlist_selected(
        locale,
        selected,
        info.items().count(),
    ));
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("playlist choices")
        .max_height(ui.available_height().max(1.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, item) in info.items().enumerate() {
                let mut included = choices.includes(index);
                // Truncation keeps long titles from widening the native window.
                let response = ui.add(egui::Checkbox::new(&mut included, item.title()));
                if response.clicked() {
                    action = Some(EditingAction::PlaylistItem(index));
                }
                response.on_hover_text(item.title());
            }
        });
    action
}

fn editing_contents(
    ui: &mut egui::Ui,
    locale: Locale,
    form: &Form,
    downloads: Option<&Downloads>,
) -> Option<EditingAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        if ui
            .selectable_label(form.source() == Source::Files, Text::FileTab.get(locale))
            .clicked()
        {
            action = Some(EditingAction::Source(Source::Files));
        }
        if ui
            .selectable_label(form.source() == Source::Url, Text::UrlTab.get(locale))
            .clicked()
        {
            action = Some(EditingAction::Source(Source::Url));
        }
    });
    ui.separator();
    ui.add_space(ROW_SPACING);
    egui::Grid::new("form")
        .num_columns(2)
        .min_col_width(FORM_LABEL_WIDTH)
        .spacing([COLUMN_SPACING, ROW_SPACING])
        .show(ui, |ui| {
            for row in [source_row, output_row] {
                // At most one widget is clicked in a frame, so a later row
                // never overwrites an earlier one's action.
                action = row(ui, locale, form, downloads).or(action.take());
                ui.end_row();
            }
        });

    if let Some(problem) = form.problem() {
        ui.add_space(SECTION_SPACING);
        show_problem(ui, locale, problem);
    }
    ui.add_space(SECTION_SPACING);
    ui.horizontal(|ui| {
        let marker = match form.separation_panel() {
            SeparationPanel::Collapsed => DISCLOSURE_CLOSED,
            SeparationPanel::Expanded => DISCLOSURE_OPEN,
        };
        let heading = format!("{marker} {}", Text::SeparationSettings.get(locale));
        if ui
            .add(egui::Button::new(egui::RichText::new(heading).strong()).frame(false))
            .clicked()
        {
            action = Some(EditingAction::SeparationPanel(
                form.separation_panel().toggled(),
            ));
        }
        action = ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let language = language_menu(ui, locale);
                let button = ui.add_enabled(
                    !downloads.is_some_and(Downloads::is_clearing),
                    egui::Button::new(egui::RichText::new("\u{2699}").size(GLOBE_SIZE).weak())
                        .frame(false),
                );
                let button = name_picture(button, Text::Settings.get(locale))
                    .on_hover_text(Text::Settings.get(locale));
                if button.clicked() {
                    Some(EditingAction::Settings)
                } else {
                    language
                }
            })
            .inner
            .or(action.take());
    });
    match form.separation_panel() {
        SeparationPanel::Collapsed => {}
        SeparationPanel::Expanded => {
            ui.add_space(ROW_SPACING);
            action = separation_controls(ui, locale, form, downloads).or(action);
        }
    }
    action
}

fn start_block(form: &Form, downloads: Option<&Downloads>, mode: Mode) -> Option<StartBlock> {
    if downloads.is_some_and(Downloads::is_clearing) {
        Some(StartBlock::ClearingCache)
    } else {
        form.start_block(mode)
    }
}

fn separation_controls(
    ui: &mut egui::Ui,
    locale: Locale,
    form: &Form,
    downloads: Option<&Downloads>,
) -> Option<EditingAction> {
    let mut action = None;
    egui::Grid::new("separation settings")
        .num_columns(2)
        .min_col_width(FORM_LABEL_WIDTH)
        .spacing([COLUMN_SPACING, ROW_SPACING])
        .show(ui, |ui| {
            for row in [model_row, stems_row, cleanup_row, device_row] {
                action = row(ui, locale, form, downloads).or(action.take());
                ui.end_row();
            }
        });
    ui.add_space(SECTION_SPACING);
    let split_block = start_block(form, downloads, Mode::Split);
    let mix_block = start_block(form, downloads, Mode::SplitAndMix);
    ui.horizontal(|ui| {
        let split = egui::Button::new(egui::RichText::new(Text::Split.get(locale)).strong())
            .selected(true)
            .min_size(egui::vec2(130.0, 34.0));
        if ui.add_enabled(split_block.is_none(), split).clicked() {
            action = Some(EditingAction::Start(Mode::Split));
        }
        let mix = egui::Button::new(egui::RichText::new(Text::SplitAndMix.get(locale)).strong())
            .selected(true)
            .min_size(egui::vec2(160.0, 34.0));
        let mix = ui.add_enabled(mix_block.is_none(), mix);
        let mix = if form.can_mix() {
            mix
        } else {
            mix.on_disabled_hover_text(Text::NoStemsForMix.get(locale))
        };
        if mix.clicked() {
            action = Some(EditingAction::Start(Mode::SplitAndMix));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut format = form.output_format();
            egui::ComboBox::from_id_salt("result encoding")
                .width(85.0)
                .selected_text(text::output_format(locale, format))
                .show_ui(ui, |ui| {
                    for &choice in stem_split_core::domain::OutputFormat::ALL {
                        ui.selectable_value(
                            &mut format,
                            choice,
                            text::output_format(locale, choice),
                        );
                    }
                })
                .response
                .on_hover_text(Text::OutputFormat.get(locale));
            if format != form.output_format() {
                action = Some(EditingAction::OutputFormat(format));
            }
        });
    });
    // Keep explanations visible without changing the window height when a
    // playlist selection becomes empty or valid again.
    let reasons = [
        split_block,
        mix_block.filter(|reason| Some(*reason) != split_block),
    ]
    .into_iter()
    .flatten()
    .map(|reason| text::start_block(locale, reason))
    .collect::<Vec<_>>()
    .join(" · ");
    let height = ui.text_style_height(&egui::TextStyle::Body);
    ui.allocate_ui_with_layout(
        egui::vec2(FORM_CONTENT_WIDTH, height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_min_height(height);
            if !reasons.is_empty() {
                ui.add(egui::Label::new(egui::RichText::new(&reasons).weak()).truncate())
                    .on_hover_text(reasons);
            }
        },
    );
    action
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
        Progress::Separating { done, total, .. } | Progress::Enhancing { done, total, .. } => {
            Some(done as f32 / total.get() as f32)
        }
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
            RowStatus::Downloading(progress) => Self {
                fraction: media_fraction(progress),
                text: text::media_status(locale, progress),
                fill: None,
            },
            RowStatus::Downloaded => still(1.0, Text::DownloadComplete),
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
                if let Some(failure) = row.failure {
                    show_message(
                        ui,
                        &text::failure(locale, failure),
                        ui.visuals().error_fg_color,
                    );
                }
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

#[expect(
    clippy::cast_precision_loss,
    reason = "download progress is displayed with f32 precision"
)]
fn media_fraction(progress: stem_split_media::Progress) -> Option<f32> {
    match progress {
        stem_split_media::Progress::Downloading { downloaded, total } => {
            total.map(|total| (downloaded as f32 / total.get() as f32).clamp(0.0, 1.0))
        }
        stem_split_media::Progress::InstallingDownloader
        | stem_split_media::Progress::InstallingFfmpeg
        | stem_split_media::Progress::Inspecting
        | stem_split_media::Progress::Converting => None,
    }
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
                    Outcome::Downloaded => {
                        ui.label(Text::DownloadComplete.get(locale));
                    }
                    Outcome::Completed { output, took } => {
                        ui.weak(text::finished_summary(locale, *took, output.device()));
                        let names: Vec<String> = output
                            .mix()
                            .into_iter()
                            .chain(output.files().iter().map(|(_, path)| path.as_path()))
                            .map(text::file_name)
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
    let heading = match failure {
        Failure::Media(_) => Text::DownloadFailed.get(locale).to_owned(),
        Failure::Job(_) | Failure::NotStarted(_) | Failure::Crashed => {
            text::results_heading(locale, 0, files)
        }
    };
    ui.heading(heading);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsAction {
    Back,
    OpenCache,
    PickCache,
    DefaultCache,
    Parallel(std::num::NonZeroUsize),
    Tool(stem_split_media::Tool, stem_split_media::tools::Action),
    Cancel,
}

pub(crate) fn settings(
    ui: &mut egui::Ui,
    locale: Locale,
    form: &Form,
    tools: &crate::tools::Tools,
    downloads: Option<&Downloads>,
) -> Option<SettingsAction> {
    use stem_split_media::{Tool, tools::Action};
    let mut action = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading(Text::Settings.get(locale));
        ui.add_space(SECTION_SPACING);
        ui.horizontal(|ui| {
            ui.label(Text::ParallelTracks.get(locale));
            let mut tracks = form.parallel_tracks().get();
            if ui
                .add(egui::DragValue::new(&mut tracks).range(1..=8))
                .changed()
                && let Some(tracks) = std::num::NonZeroUsize::new(tracks)
            {
                action = Some(SettingsAction::Parallel(tracks));
            }
        });
        ui.weak(Text::ParallelTracksHint.get(locale));
        ui.add_space(SECTION_SPACING);
        ui.separator();
        ui.heading(Text::MediaTools.get(locale));
        ui.weak(Text::ToolsHint.get(locale));
        ui.add_space(ROW_SPACING);
        let available =
            downloads.is_some_and(|downloads| !downloads.is_clearing()) && !tools.busy();
        for &tool in Tool::ALL {
            ui.push_id(tool.name(), |ui| {
                ui.horizontal(|ui| {
                    ui.strong(text::tool_name(locale, tool));
                    let installed = tools
                        .installed()
                        .is_some_and(|installed| installed.contains(tool));
                    let status = if installed {
                        Text::ToolInstalled
                    } else {
                        Text::ToolMissing
                    };
                    ui.weak(status.get(locale));
                    ui.add_enabled_ui(available && tools.installed().is_some(), |ui| {
                        if installed {
                            if ui.button(Text::ToolUpdate.get(locale)).clicked() {
                                action = Some(SettingsAction::Tool(tool, Action::Update));
                            }
                            if ui.button(Text::Delete.get(locale)).clicked() {
                                action = Some(SettingsAction::Tool(tool, Action::Remove));
                            }
                        } else if ui.button(Text::Download.get(locale)).clicked() {
                            action = Some(SettingsAction::Tool(tool, Action::Install));
                        }
                    });
                });
            });
        }
        action = cache_controls(
            ui,
            locale,
            form.cache_location(),
            downloads,
            if tools.busy() || form.preview_busy() {
                CacheAccess::Busy
            } else {
                CacheAccess::Available
            },
        )
        .or(action);
        if tools.busy() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(Text::ToolBusy.get(locale));
                if ui.button(Text::Cancel.get(locale)).clicked() {
                    action = Some(SettingsAction::Cancel);
                }
            });
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(problem) = tools.problem() {
            ui.colored_label(ui.visuals().error_fg_color, Text::ToolFailed.get(locale));
            ui.weak(problem.to_string());
        }
        ui.add_space(SECTION_SPACING);
        if ui
            .add_enabled(!tools.busy(), egui::Button::new(Text::Back.get(locale)))
            .clicked()
        {
            action = Some(SettingsAction::Back);
        }
    });
    action
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheAccess {
    Available,
    Busy,
}

fn cache_controls(
    ui: &mut egui::Ui,
    locale: Locale,
    location: &crate::settings::CacheLocation,
    downloads: Option<&Downloads>,
    access: CacheAccess,
) -> Option<SettingsAction> {
    let mut action = None;
    ui.add_space(SECTION_SPACING);
    ui.separator();
    ui.heading(Text::CacheFolder.get(locale));
    if let Some(downloads) = downloads {
        show_folder(ui, downloads.directory());
    } else {
        ui.weak(Text::ToolUnavailable.get(locale));
    }
    ui.weak(Text::CacheFolderHint.get(locale));
    ui.add_enabled_ui(access == CacheAccess::Available, |ui| {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    downloads.is_some(),
                    egui::Button::new(Text::OpenFolder.get(locale)),
                )
                .clicked()
            {
                action = Some(SettingsAction::OpenCache);
            }
            if ui.button(Text::Browse.get(locale)).clicked() {
                action = Some(SettingsAction::PickCache);
            }
            if ui
                .add_enabled(
                    matches!(location, crate::settings::CacheLocation::Custom(_)),
                    egui::Button::new(Text::DefaultCache.get(locale)),
                )
                .clicked()
            {
                action = Some(SettingsAction::DefaultCache);
            }
        });
    });
    action
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use stem_split_core::separate::ActiveDevice;

    use super::*;

    #[test]
    fn settings_controls_fit_in_both_languages() -> Result<(), io::Error> {
        for &locale in Locale::ALL {
            let context = egui::Context::default();
            for _ in 0..3 {
                let output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(640.0, 540.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::default().show(ui, |ui| {
                            settings(
                                ui,
                                locale,
                                &Form::default(),
                                &crate::tools::Tools::default(),
                                None,
                            );
                        });
                    },
                );
                for label in [
                    Text::Settings,
                    Text::ParallelTracks,
                    Text::ParallelTracksHint,
                    Text::MediaTools,
                    Text::CacheFolder,
                    Text::Back,
                ] {
                    let visible = output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::epaint::Shape::Text(text)
                                if text.galley.job.text == label.get(locale) =>
                            {
                                Some((
                                    shape.clip_rect,
                                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                                ))
                            }
                            _ => None,
                        })
                        .ok_or_else(|| io::Error::other("settings control missing"))?;
                    assert!(
                        visible.0.expand(0.5).contains_rect(visible.1),
                        "{locale:?}: {label:?} is clipped"
                    );
                }
                output.drop_without_applying_deltas();
            }
        }
        Ok(())
    }

    #[test]
    fn playlist_side_panel_adds_width_without_height() -> Result<(), io::Error> {
        use crate::{playlist::Panel, preview::State};
        use stem_split_media::{MediaInfo, MediaItem, MediaUrl};
        let directory = tempfile::tempdir()?;
        let url = MediaUrl::new("https://example.org/list").map_err(io::Error::other)?;
        for &locale in Locale::ALL {
            for separation in [SeparationPanel::Collapsed, SeparationPanel::Expanded] {
                let mut form = Form::default();
                form.set_source(Source::Url);
                form.set_url(url.as_str().to_owned());
                form.set_output_folder(directory.path());
                form.set_separation_panel(separation);
                form.show_preview(State::Ready(MediaInfo::Playlist {
                    title: "Playlist".to_owned(),
                    first: MediaItem::new("Track 0".to_owned(), url.clone()),
                    rest: (1..100)
                        .map(|index| {
                            MediaItem::new(format!("Track {index} ").repeat(40), url.clone())
                        })
                        .collect(),
                }));
                let context = egui::Context::default();
                let measure = |form: &Form| {
                    let mut minimum = egui::Vec2::ZERO;
                    for _ in 0..3 {
                        let output = context.run_ui(
                            egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(
                                    egui::Pos2::ZERO,
                                    egui::vec2(960.0, 540.0),
                                )),
                                ..Default::default()
                            },
                            |ui| {
                                minimum = editing(ui, locale, form, None).minimum;
                            },
                        );
                        output.drop_without_applying_deltas();
                    }
                    minimum
                };
                let closed = measure(&form);
                form.set_playlist_panel(Panel::Open);
                let open = measure(&form);
                assert!(
                    (open.y - closed.y).abs() < 0.5,
                    "{locale:?}: {closed:?} versus {open:?}"
                );
                assert!((open.x - closed.x - PLAYLIST_WIDTH - PLAYLIST_GAP).abs() < 0.5);
                for index in 0..100 {
                    form.toggle_playlist_item(index);
                }
                assert!((measure(&form).y - closed.y).abs() < 0.5);
            }
        }
        Ok(())
    }

    #[test]
    fn metadata_and_long_playlists_keep_the_form_height() -> Result<(), io::Error> {
        use crate::preview::State;
        use stem_split_media::{MediaInfo, MediaItem, MediaUrl};

        let url = MediaUrl::new("https://example.org/list").map_err(io::Error::other)?;
        for &locale in Locale::ALL {
            for panel in [SeparationPanel::Collapsed, SeparationPanel::Expanded] {
                let mut form = Form::default();
                form.set_source(Source::Url);
                form.set_url(url.as_str().to_owned());
                form.set_separation_panel(panel);
                let context = egui::Context::default();
                let measure = |form: &Form| {
                    let mut size = egui::Vec2::ZERO;
                    for _ in 0..3 {
                        let output = context.run_ui(
                            egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(
                                    egui::Pos2::ZERO,
                                    egui::vec2(640.0, 700.0),
                                )),
                                ..Default::default()
                            },
                            |ui| {
                                size = editing(ui, locale, form, None).minimum;
                            },
                        );
                        output.drop_without_applying_deltas();
                    }
                    size
                };
                let empty = measure(&form);
                form.show_preview(State::Loading(url.clone()));
                assert!((empty - measure(&form)).length() < 0.5);
                form.show_preview(State::Failed(Failure::Crashed));
                assert!((empty - measure(&form)).length() < 0.5);
                form.show_preview(State::Ready(MediaInfo::Single(MediaItem::new(
                    "Long title ".repeat(100),
                    url.clone(),
                ))));
                assert!((empty - measure(&form)).length() < 0.5);
                for count in [2, 100] {
                    form.show_preview(State::Ready(MediaInfo::Playlist {
                        title: "Playlist title ".repeat(100),
                        first: MediaItem::new("First track".to_owned(), url.clone()),
                        rest: (1..count)
                            .map(|number| MediaItem::new(format!("Track {number}"), url.clone()))
                            .collect(),
                    }));
                    assert!(
                        (empty - measure(&form)).length() < 0.5,
                        "{locale:?}, {panel:?}, {count}"
                    );
                }
            }
        }
        Ok(())
    }

    fn model_position(form: &Form, locale: Locale, width: f32) -> Result<egui::Pos2, io::Error> {
        let context = egui::Context::default();
        let mut position = None;
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 520.0),
                )),
                ..Default::default()
            };
            let output = context.run_ui(input, |ui| {
                let _action = editing(ui, locale, form, None);
            });
            position = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text)
                    if text.galley.job.text == Text::Model.get(locale) =>
                {
                    Some(text.pos)
                }
                _ => None,
            });
            output.drop_without_applying_deltas();
        }
        position.ok_or_else(|| io::Error::other("the model heading was not rendered"))
    }

    #[test]
    fn input_tabs_keep_common_settings_in_place() -> Result<(), io::Error> {
        let directory = tempfile::tempdir()?;
        let mut paths = Vec::new();
        for number in 0..6 {
            let path = directory.path().join(format!("track-{number}.wav"));
            std::fs::write(&path, b"")?;
            paths.push(path);
        }
        for selection in [None, Some(paths.as_slice())] {
            let mut form = Form::default();
            if let Some(paths) = selection {
                form.set_inputs(paths);
            }
            form.set_output_folder(directory.path());
            for &locale in Locale::ALL {
                for width in [520.0, 640.0] {
                    form.set_source(Source::Files);
                    let files = model_position(&form, locale, width)?;
                    form.set_source(Source::Url);
                    form.set_url("invalid link".to_owned());
                    let url = model_position(&form, locale, width)?;
                    assert!(
                        files.distance(url) < 0.5,
                        "{locale:?}, {width}: {files:?} versus {url:?}"
                    );
                }
            }
        }
        Ok(())
    }

    fn check_form_fits(form: &Form, locale: Locale) -> Result<(), io::Error> {
        let context = egui::Context::default();
        let mut size = egui::vec2(640.0, 540.0);
        for pass in 0..4 {
            let previous = size;
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                },
                |ui| {
                    size = egui::CentralPanel::default()
                        .show(ui, |ui| editing(ui, locale, form, None))
                        .inner
                        .minimum;
                },
            );
            if pass == 3 {
                assert!(
                    (size - previous).length() < 0.5,
                    "window size must settle: {previous:?}, {size:?}"
                );
                let mut last_button = 0.0_f32;
                for label in [Text::Split, Text::SplitAndMix, Text::Device, Text::Wav] {
                    let (clip, rect) = output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::epaint::Shape::Text(text)
                                if text.galley.job.text == label.get(locale) =>
                            {
                                Some((
                                    shape.clip_rect,
                                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                                ))
                            }
                            _ => None,
                        })
                        .ok_or_else(|| io::Error::other("a required control was not rendered"))?;
                    assert!(
                        clip.expand(0.5).contains_rect(rect),
                        "{label:?} must be fully visible: {clip:?}, {rect:?}"
                    );
                    last_button = last_button.max(rect.bottom());
                }
                assert!(
                    size.y - last_button < 40.0,
                    "the footer must remain compact"
                );
            }
            output.drop_without_applying_deltas();
        }
        Ok(())
    }

    #[test]
    fn measured_window_fits_controls_without_a_large_footer() -> Result<(), io::Error> {
        let directory = tempfile::tempdir()?;
        let mut paths = Vec::new();
        for number in 0..7 {
            let path = directory.path().join(format!("track-{number}.wav"));
            std::fs::write(&path, b"")?;
            paths.push(path);
        }
        let mut form = Form::default();
        form.set_inputs(&paths);
        form.set_output_folder(directory.path());
        form.set_model(
            ModelId::Htdemucs6s,
            Some(&stem_split_core::model::ModelStore::new(
                directory.path().join("models"),
            )),
        );
        form.set_cleanup(true, None);
        for &locale in Locale::ALL {
            check_form_fits(&form, locale)?;
        }
        form.set_source(Source::Url);
        form.set_url("https://example.org/audio".to_owned());
        form.set_output_folder(directory.path());
        for &locale in Locale::ALL {
            check_form_fits(&form, locale)?;
        }
        Ok(())
    }

    #[test]
    fn collapsed_url_form_keeps_download_beside_clear_and_hides_separation() -> Result<(), io::Error>
    {
        let directory = tempfile::tempdir()?;
        let mut form = Form::default();
        let mut files = Vec::new();
        for number in 0..6 {
            let path = directory.path().join(format!("track-{number}.wav"));
            std::fs::write(&path, b"")?;
            files.push(path);
        }
        form.set_inputs(&files);
        form.set_source(Source::Url);
        form.set_url("https://example.org/audio".to_owned());
        form.set_output_folder(directory.path());
        form.set_separation_panel(SeparationPanel::Collapsed);
        for &locale in Locale::ALL {
            let context = egui::Context::default();
            for pass in 0..3 {
                let mut minimum = egui::Vec2::ZERO;
                let output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(640.0, 400.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        minimum = editing(ui, locale, &form, None).minimum;
                    },
                );
                if pass == 2 {
                    let position = |label: &str| {
                        output.shapes.iter().find_map(|shape| match &shape.shape {
                            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                                Some(text.pos)
                            }
                            _ => None,
                        })
                    };
                    let clear = position(CLEAR_INPUT)
                        .ok_or_else(|| io::Error::other("clear control was not rendered"))?;
                    let download = position(Text::Download.get(locale))
                        .ok_or_else(|| io::Error::other("download control was not rendered"))?;
                    assert!(
                        (clear.y - download.y).abs() < 1.0
                            && download.x > clear.x
                            && download.x - clear.x < 80.0
                    );
                    assert!(position(Text::Url.get(locale)).is_some());
                    assert!(position(Text::Output.get(locale)).is_some());
                    let browse = position(Text::Browse.get(locale))
                        .ok_or_else(|| io::Error::other("folder button missing"))?;
                    let format = position(Text::OriginalFormat.get(locale))
                        .ok_or_else(|| io::Error::other("source format missing"))?;
                    assert!(format.x > browse.x && (format.y - browse.y).abs() < 2.0);

                    for hidden in [
                        Text::Model,
                        Text::Stems,
                        Text::Cleanup,
                        Text::Device,
                        Text::Split,
                        Text::SplitAndMix,
                    ] {
                        assert!(
                            position(hidden.get(locale)).is_none(),
                            "{hidden:?} must be hidden"
                        );
                    }
                    assert!(
                        minimum.y < 280.0,
                        "collapsed form must stay compact: {minimum:?}"
                    );
                }
                output.drop_without_applying_deltas();
            }
        }
        Ok(())
    }

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
