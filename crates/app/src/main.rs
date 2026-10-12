//! Desktop application for splitting audio into stems.

mod app;
mod view;

use eframe::egui;

use crate::{
    app::StemSplitApp,
    view::text::{Locale, Text},
};

/// Identifies the application to the operating system. It is not shown to the
/// user and must not change with the interface language.
const APP_ID: &str = "stem-split";

fn main() -> eframe::Result {
    let locale = Locale::default();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(Text::WindowTitle.get(locale))
            .with_inner_size([560.0, 360.0])
            .with_min_inner_size([420.0, 260.0]),
        ..Default::default()
    };
    eframe::run_native(
        APP_ID,
        options,
        Box::new(move |_creation| Ok(Box::new(StemSplitApp::new(locale)))),
    )
}
