//! Native file and folder pickers.
//!
//! The dialogs are modal: the window does not react while one is open. They
//! are only reachable from the main form, never while a job runs.

use std::path::PathBuf;

use stem_split_core::domain::AudioFormat;

use crate::view::text::{Locale, Text};

/// Asks for an audio file, offering only the supported formats.
pub(crate) fn pick_audio_file(locale: Locale, parent: &eframe::Frame) -> Option<PathBuf> {
    let extensions: Vec<&str> = AudioFormat::all_extensions().collect();
    rfd::FileDialog::new()
        .add_filter(Text::AudioFiles.get(locale), &extensions)
        .set_parent(parent)
        .pick_file()
}

/// Asks for a folder.
pub(crate) fn pick_folder(parent: &eframe::Frame) -> Option<PathBuf> {
    rfd::FileDialog::new().set_parent(parent).pick_folder()
}
