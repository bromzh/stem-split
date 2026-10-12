//! Rendering. Functions here turn state into widgets and hold no state of
//! their own. Every string they show comes from [`text`].

pub(crate) mod text;

use eframe::egui;
use stem_split_core::domain::ModelId;

use self::text::{Locale, Text};

/// Placeholder content until the separation form exists: the models the
/// application will offer.
pub(crate) fn show(ui: &mut egui::Ui, locale: Locale) {
    ui.heading(Text::AvailableModels.get(locale));
    ui.add_space(4.0);
    for &model in ModelId::ALL {
        ui.label(text::model_label(locale, model));
    }
}
