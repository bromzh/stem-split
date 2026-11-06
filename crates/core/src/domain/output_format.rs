use crate::closed_enum;

closed_enum! {
    /// Encoding of a published stem or mix.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum OutputFormat {
        #[default]
        Wav,
        Mp3,
    }
}

impl OutputFormat {
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|format| format.extension() == key)
    }
}
