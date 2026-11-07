//! Cached `FFmpeg` encoding shared by source downloads and separated outputs.

use stem_split_audio::{closed_enum, domain::OutputFormat};

closed_enum! {
    /// How downloaded material is saved in the selected destination.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum SaveFormat { #[default] Original, Wav, Mp3 }
}

impl SaveFormat {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|format| format.key() == key)
    }

    pub(super) const fn encoding(self) -> Option<OutputFormat> {
        match self {
            Self::Original => None,
            Self::Wav => Some(OutputFormat::Wav),
            Self::Mp3 => Some(OutputFormat::Mp3),
        }
    }
}
