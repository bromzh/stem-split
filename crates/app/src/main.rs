//! Desktop application for splitting audio into stems.

mod app;
mod dialogs;
mod form;
mod logging;
mod timing;
mod view;
mod worker;

use std::{env, path::PathBuf};

use eframe::egui;

use crate::{
    app::StemSplitApp,
    view::text::{Locale, Text},
};

/// Identifies the application to the operating system. It is not shown to the
/// user and must not change with the interface language.
const APP_ID: &str = "stem-split";

fn main() -> eframe::Result {
    logging::install();
    let locale = Locale::default();
    // A file passed on the command line, as when a file is opened with the
    // application, is what the form starts with.
    let input = env::args_os().nth(1).map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(Text::WindowTitle.get(locale))
            .with_inner_size([640.0, 400.0])
            .with_min_inner_size([520.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        APP_ID,
        options,
        Box::new(move |_creation| Ok(Box::new(StemSplitApp::new(locale, input.as_deref())))),
    )
}
