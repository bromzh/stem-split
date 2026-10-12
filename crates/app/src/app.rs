use eframe::egui;

use crate::view::{self, text::Locale};

/// Application state. Until the separation screens exist, it is only the
/// interface language.
#[derive(Debug)]
pub(crate) struct StemSplitApp {
    locale: Locale,
}

impl StemSplitApp {
    pub(crate) const fn new(locale: Locale) -> Self {
        Self { locale }
    }
}

impl eframe::App for StemSplitApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| view::show(ui, self.locale));
    }
}
