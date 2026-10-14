//! Native file and folder pickers.
//!
//! The dialogs are modal: the window does not react while one is open. They
//! are only reachable from the main form, never while a job runs.

use std::path::PathBuf;

use stem_split_core::domain::AudioFormat;

use crate::view::text::{self, Locale, Text};

/// Asks for one or more audio files, offering only the supported formats.
pub(crate) fn pick_audio_files(locale: Locale, parent: &eframe::Frame) -> Option<Vec<PathBuf>> {
    let extensions: Vec<&str> = AudioFormat::all_extensions().collect();
    rfd::FileDialog::new()
        .add_filter(Text::AudioFiles.get(locale), &extensions)
        .set_parent(parent)
        .pick_files()
}

/// Tells the user, in a message box, that the application could not start.
pub(crate) fn report_start_failure(locale: Locale, error: &eframe::Error) {
    let message = text::start_failure(locale, error);
    let description = match message.detail {
        Some(detail) => format!("{}\n\n{detail}", message.summary),
        None => message.summary,
    };
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(Text::WindowTitle.get(locale))
        .set_description(description)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// Asks for a folder.
pub(crate) fn pick_folder(parent: &eframe::Frame) -> Option<PathBuf> {
    rfd::FileDialog::new().set_parent(parent).pick_folder()
}
