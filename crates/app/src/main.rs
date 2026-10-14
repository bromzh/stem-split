//! Desktop application for splitting audio into stems.

// A release build is a window and nothing else: no console next to it.
// Debug builds keep the console, where warnings are easy to watch.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod dialogs;
mod downloads;
mod form;
mod logging;
mod settings;
mod timing;
mod tracker;
mod view;
mod worker;

use std::{env, path::PathBuf, process::ExitCode};

use eframe::egui;

use crate::{
    app::StemSplitApp,
    settings::Settings,
    view::text::{Locale, Text},
};

/// Identifies the application to the operating system and names the folders
/// its settings and log are kept in. It is not shown to the user and must
/// not change with the interface language.
pub(crate) const APP_ID: &str = "stem-split";

/// The picture drawn by `assets/make_icon.py`, as raw RGBA.
const ICON: &[u8] = include_bytes!("../assets/icon-64.rgba");
const ICON_SIZE: u32 = 64;

fn run() -> eframe::Result {
    // Files passed on the command line, as when files are opened with the
    // application, are what the form starts with.
    let inputs: Vec<PathBuf> = env::args_os().skip(1).map(PathBuf::from).collect();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // The name is the same in every language, so the title does not
            // have to wait for the saved language to be read.
            .with_title(Text::WindowTitle.get(Locale::default()))
            .with_icon(egui::IconData {
                rgba: ICON.to_vec(),
                width: ICON_SIZE,
                height: ICON_SIZE,
            })
            .with_inner_size([640.0, 400.0])
            .with_min_inner_size([520.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        APP_ID,
        options,
        Box::new(move |creation| {
            let settings = creation
                .storage
                .map_or_else(Settings::initial, Settings::load);
            Ok(Box::new(StemSplitApp::new(&settings, &inputs)))
        }),
    )
}

fn main() -> ExitCode {
    logging::install();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            log::error!("the application could not start: {error}");
            // Without a console this dialog is the only way the user learns
            // why no window appeared. The saved language is out of reach
            // here, so the system's is used.
            dialogs::report_start_failure(Locale::of_system(), &error);
            ExitCode::FAILURE
        }
    }
}
