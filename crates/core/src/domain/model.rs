use super::{macros::closed_enum, stem::StemLayout};

closed_enum! {
    /// Separation models the application can run.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub enum ModelId {
        /// Four stems from a single network. Fastest, and the default choice.
        #[default]
        Htdemucs,
        /// Four stems, each from a network fine-tuned for it: somewhat better
        /// quality at about four times the processing time.
        HtdemucsFt,
        /// Six stems: the usual four plus guitar and piano.
        Htdemucs6s,
    }
}

impl ModelId {
    /// The name the model is published under. It is an identifier, not a
    /// label: it stays the same in every interface language.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Htdemucs => "htdemucs",
            Self::HtdemucsFt => "htdemucs_ft",
            Self::Htdemucs6s => "htdemucs_6s",
        }
    }

    /// The stems the model produces.
    #[must_use]
    pub const fn layout(self) -> StemLayout {
        match self {
            Self::Htdemucs | Self::HtdemucsFt => StemLayout::FOUR,
            Self::Htdemucs6s => StemLayout::SIX,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn names_are_distinct() {
        let names: HashSet<&str> = ModelId::ALL.iter().map(|model| model.name()).collect();
        assert_eq!(names.len(), ModelId::ALL.len());
    }

    #[test]
    fn default_is_the_single_network_model() {
        assert_eq!(ModelId::default(), ModelId::Htdemucs);
        assert_eq!(ModelId::ALL.first(), Some(&ModelId::Htdemucs));
    }

    #[test]
    fn layouts_match_the_models() {
        assert_eq!(ModelId::Htdemucs.layout(), StemLayout::FOUR);
        assert_eq!(ModelId::HtdemucsFt.layout(), StemLayout::FOUR);
        assert_eq!(ModelId::Htdemucs6s.layout(), StemLayout::SIX);
    }
}
