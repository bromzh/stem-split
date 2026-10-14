use std::ffi::OsStr;

use crate::closed_enum;

closed_enum! {
    /// Input formats the application accepts, recognized by file extension.
    ///
    /// The extension only decides whether a file is offered to the decoder;
    /// the decoder identifies the actual content itself.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum AudioFormat {
        Mp3,
        Flac,
        Wav,
        Ogg,
        M4a,
    }
}

impl AudioFormat {
    /// File extensions of the format: lowercase, without the dot.
    #[must_use]
    pub const fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Mp3 => &["mp3"],
            Self::Flac => &["flac"],
            Self::Wav => &["wav", "wave"],
            Self::Ogg => &["ogg", "oga"],
            Self::M4a => &["m4a"],
        }
    }

    /// Extensions of every supported format, for file dialog filters.
    pub fn all_extensions() -> impl Iterator<Item = &'static str> {
        Self::ALL
            .iter()
            .flat_map(|format| format.extensions().iter().copied())
    }

    /// Recognizes a format by extension, ignoring ASCII case.
    #[must_use]
    pub fn from_extension(extension: &OsStr) -> Option<Self> {
        let extension = extension.to_str()?;
        Self::ALL.iter().copied().find(|format| {
            format
                .extensions()
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn extensions_are_recognized_in_any_case() {
        assert_eq!(
            AudioFormat::from_extension(OsStr::new("mp3")),
            Some(AudioFormat::Mp3)
        );
        assert_eq!(
            AudioFormat::from_extension(OsStr::new("FLAC")),
            Some(AudioFormat::Flac)
        );
        assert_eq!(
            AudioFormat::from_extension(OsStr::new("Wave")),
            Some(AudioFormat::Wav)
        );
    }

    #[test]
    fn unknown_extensions_are_rejected() {
        assert_eq!(AudioFormat::from_extension(OsStr::new("txt")), None);
        assert_eq!(AudioFormat::from_extension(OsStr::new("")), None);
        assert_eq!(AudioFormat::from_extension(OsStr::new(".mp3")), None);
    }

    #[test]
    fn every_extension_maps_back_to_its_format() {
        for &format in AudioFormat::ALL {
            for extension in format.extensions() {
                assert_eq!(
                    AudioFormat::from_extension(OsStr::new(extension)),
                    Some(format)
                );
            }
        }
    }

    #[test]
    fn extensions_are_distinct_lowercase_ascii() {
        let all: Vec<&str> = AudioFormat::all_extensions().collect();
        let unique: HashSet<&str> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len());
        for extension in all {
            assert!(!extension.is_empty());
            assert!(
                extension
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            );
        }
    }
}
