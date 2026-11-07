//! Commands shared by the headless workflow and its callers.

/// Whether URL preparation should stop at the saved audio or keep going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlAction {
    Download,
    Separate(Mode),
}

/// Whether separation also produces a mix of selected stems.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A file for every stem.
    Split,
    /// A mix of the ticked stems, and the stems too if they are kept.
    SplitAndMix,
}
