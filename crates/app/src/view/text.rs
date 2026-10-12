//! Every user-visible string, for every interface language.
//!
//! Translations are picked by exhaustive `match`, so adding a [`Locale`] or a
//! [`Text`] does not compile until each string exists in each language.

use stem_split_core::domain::ModelId;

/// Interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Locale {
    #[default]
    En,
}

/// A user-visible string without parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Text {
    WindowTitle,
    AvailableModels,
}

impl Text {
    pub(crate) const fn get(self, locale: Locale) -> &'static str {
        match locale {
            Locale::En => match self {
                Self::WindowTitle => "stem-split",
                Self::AvailableModels => "Available models",
            },
        }
    }
}

/// One line describing a model, such as `htdemucs — 4 stems, fast`.
pub(crate) fn model_label(locale: Locale, model: ModelId) -> String {
    let name = model.name();
    let stems = model.layout().count();
    match locale {
        Locale::En => {
            let note = match model {
                ModelId::Htdemucs => "fast",
                ModelId::HtdemucsFt => "best quality, about 4x slower",
                ModelId::Htdemucs6s => "adds guitar and piano",
            };
            format!("{name} — {stems} stems, {note}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn model_label_names_the_model_and_its_stem_count() {
        assert_eq!(
            model_label(Locale::En, ModelId::Htdemucs),
            "htdemucs — 4 stems, fast"
        );
        assert_eq!(
            model_label(Locale::En, ModelId::Htdemucs6s),
            "htdemucs_6s — 6 stems, adds guitar and piano"
        );
    }

    #[test]
    fn model_labels_are_distinct() {
        let labels: HashSet<String> = ModelId::ALL
            .iter()
            .map(|&model| model_label(Locale::En, model))
            .collect();
        assert_eq!(labels.len(), ModelId::ALL.len());
    }
}
