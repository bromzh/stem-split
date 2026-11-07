use super::{Stem, StemLayout};

/// A selection of stems, represented by a stable bit for every [`Stem`].
///
/// An empty selection is useful while editing a form. Work requests use
/// `MixStems`, which validates that the selection is non-empty and belongs to
/// the selected model layout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[must_use]
pub struct StemSet(u8);

impl StemSet {
    /// Selects no stems.
    pub const fn none() -> Self {
        Self(0)
    }

    /// Selects every stem the layout produces.
    pub fn all(layout: StemLayout) -> Self {
        layout.iter().fold(Self::none(), Self::toggled)
    }

    /// Returns the set with `stem` selected when it was absent, and absent
    /// when it was selected.
    pub const fn toggled(self, stem: Stem) -> Self {
        Self(self.0 ^ bit(stem))
    }

    /// Whether `stem` is selected.
    #[must_use]
    pub const fn contains(self, stem: Stem) -> bool {
        self.0 & bit(stem) != 0
    }

    /// Whether the set has no selected stems.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Selected stems in the order specified by `layout`.
    pub fn iter(self, layout: StemLayout) -> impl Iterator<Item = Stem> {
        layout.iter().filter(move |&stem| self.contains(stem))
    }

    /// Whether every selected stem belongs to `layout`.
    #[must_use]
    pub fn is_subset_of(self, layout: StemLayout) -> bool {
        self.iter_all().all(|stem| layout.position(stem).is_some())
    }

    /// The selected stems that `layout` produces. A selection made for one
    /// model keeps the stems another one lacks, so that switching back
    /// restores them; this is what applies to the current model.
    pub fn within(self, layout: StemLayout) -> Self {
        self.iter(layout).fold(Self::none(), Self::toggled)
    }

    /// Every selected stem, in the order of [`Stem::ALL`].
    pub fn iter_all(self) -> impl Iterator<Item = Stem> {
        Stem::ALL
            .iter()
            .copied()
            .filter(move |&stem| self.contains(stem))
    }
}

const fn bit(stem: Stem) -> u8 {
    match stem {
        Stem::Drums => 1,
        Stem::Bass => 2,
        Stem::Other => 4,
        Stem::Vocals => 8,
        Stem::Guitar => 16,
        Stem::Piano => 32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set_has_no_stems() {
        assert!(StemSet::none().is_empty());
        assert!(StemSet::none().iter(StemLayout::SIX).next().is_none());
    }

    #[test]
    fn all_follows_the_layout() {
        let four: Vec<Stem> = StemSet::all(StemLayout::FOUR)
            .iter(StemLayout::FOUR)
            .collect();
        assert_eq!(four, [Stem::Drums, Stem::Bass, Stem::Other, Stem::Vocals]);
        assert!(StemSet::all(StemLayout::FOUR).is_subset_of(StemLayout::FOUR));
    }

    #[test]
    fn toggle_is_reversible() {
        let selected = StemSet::none().toggled(Stem::Vocals);
        assert!(selected.contains(Stem::Vocals));
        assert!(selected.toggled(Stem::Vocals).is_empty());
    }

    #[test]
    fn within_drops_the_stems_a_layout_lacks() {
        let set = StemSet::none().toggled(Stem::Piano).toggled(Stem::Vocals);

        let four = set.within(StemLayout::FOUR);

        assert!(four.contains(Stem::Vocals));
        assert!(!four.contains(Stem::Piano));
        assert_eq!(set.within(StemLayout::SIX), set);
        assert!(
            StemSet::none()
                .toggled(Stem::Guitar)
                .within(StemLayout::FOUR)
                .is_empty()
        );
    }

    #[test]
    fn six_stem_selection_is_not_a_four_stem_subset() {
        let set = StemSet::none().toggled(Stem::Piano);
        assert!(!set.is_subset_of(StemLayout::FOUR));
        assert!(set.is_subset_of(StemLayout::SIX));
    }
}
