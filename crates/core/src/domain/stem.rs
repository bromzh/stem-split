use std::{iter, num::NonZeroUsize};

use crate::closed_enum;

closed_enum! {
    /// One separated part of a mix.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum Stem {
        Drums,
        Bass,
        /// Everything the model does not assign to another stem.
        Other,
        Vocals,
        Guitar,
        Piano,
    }
}

impl Stem {
    /// Lowercase ASCII name used in output file names. It is an identifier,
    /// not a label: it stays the same in every interface language.
    #[must_use]
    pub const fn file_tag(self) -> &'static str {
        match self {
            Self::Drums => "drums",
            Self::Bass => "bass",
            Self::Other => "other",
            Self::Vocals => "vocals",
            Self::Guitar => "guitar",
            Self::Piano => "piano",
        }
    }
}

/// The stems a model produces, in the order its output tensor lists them.
///
/// Never empty: the first stem is a field of its own. The only values are the
/// constants below, so a layout always matches a real model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StemLayout {
    first: Stem,
    rest: &'static [Stem],
}

impl StemLayout {
    /// Source order of the four-stem `HTDemucs` models.
    pub const FOUR: Self = Self {
        first: Stem::Drums,
        rest: &[Stem::Bass, Stem::Other, Stem::Vocals],
    };

    /// Source order of the six-stem `HTDemucs` model.
    pub const SIX: Self = Self {
        first: Stem::Drums,
        rest: &[
            Stem::Bass,
            Stem::Other,
            Stem::Vocals,
            Stem::Guitar,
            Stem::Piano,
        ],
    };

    /// Number of stems.
    #[must_use]
    pub const fn count(self) -> NonZeroUsize {
        NonZeroUsize::MIN.saturating_add(self.rest.len())
    }

    /// Stems in model output order.
    pub fn iter(self) -> impl Iterator<Item = Stem> {
        iter::once(self.first).chain(self.rest.iter().copied())
    }

    /// Index of `stem` in the model output, if the model produces it.
    #[must_use]
    pub fn position(self, stem: Stem) -> Option<usize> {
        self.iter().position(|candidate| candidate == stem)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn all_lists_every_stem_once() {
        let unique: HashSet<Stem> = Stem::ALL.iter().copied().collect();
        assert_eq!(Stem::ALL.len(), 6);
        assert_eq!(unique.len(), Stem::ALL.len());
    }

    #[test]
    fn file_tags_are_distinct_lowercase_ascii() {
        let tags: HashSet<&str> = Stem::ALL.iter().map(|stem| stem.file_tag()).collect();
        assert_eq!(tags.len(), Stem::ALL.len());
        for tag in tags {
            assert!(!tag.is_empty());
            assert!(tag.bytes().all(|byte| byte.is_ascii_lowercase()));
        }
    }

    #[test]
    fn layouts_follow_model_output_order() {
        let four: Vec<Stem> = StemLayout::FOUR.iter().collect();
        assert_eq!(four, [Stem::Drums, Stem::Bass, Stem::Other, Stem::Vocals]);
        assert_eq!(StemLayout::FOUR.count().get(), 4);

        let six: Vec<Stem> = StemLayout::SIX.iter().collect();
        assert_eq!(six.len(), 6);
        assert_eq!(StemLayout::SIX.count().get(), 6);
        // The six-stem model extends the four-stem order instead of reshuffling it.
        assert!(six.starts_with(&four));
    }

    #[test]
    fn layouts_have_no_duplicates() {
        for layout in [StemLayout::FOUR, StemLayout::SIX] {
            let unique: HashSet<Stem> = layout.iter().collect();
            assert_eq!(unique.len(), layout.count().get());
        }
    }

    #[test]
    fn position_reports_absent_stems() {
        assert_eq!(StemLayout::FOUR.position(Stem::Drums), Some(0));
        assert_eq!(StemLayout::FOUR.position(Stem::Vocals), Some(3));
        assert_eq!(StemLayout::FOUR.position(Stem::Piano), None);
        assert_eq!(StemLayout::SIX.position(Stem::Piano), Some(5));
    }
}
