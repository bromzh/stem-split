//! Transient playlist choices, independent of persisted application settings.

use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Panel {
    #[default]
    Closed,
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Selection {
    All,
    None,
}

#[derive(Debug, Default)]
pub(crate) struct Choices {
    excluded: BTreeSet<usize>,
    panel: Panel,
}

impl Choices {
    pub(crate) const fn panel(&self) -> Panel {
        self.panel
    }

    pub(crate) fn includes(&self, index: usize) -> bool {
        !self.excluded.contains(&index)
    }

    pub(crate) fn select(&mut self, selection: Selection, count: usize) {
        self.excluded = match selection {
            Selection::All => BTreeSet::new(),
            Selection::None => (0..count).collect(),
        };
    }

    pub(crate) fn toggle(&mut self, index: usize) {
        if !self.excluded.remove(&index) {
            self.excluded.insert(index);
        }
    }

    pub(crate) const fn set_panel(&mut self, panel: Panel) {
        self.panel = panel;
    }

    /// An already visible list needs no extra start interception.
    pub(crate) fn review_before_start(&mut self, count: usize) -> bool {
        if count > 5 && self.panel == Panel::Closed {
            self.panel = Panel::Open;
            true
        } else {
            false
        }
    }
}
