//! Metadata-only extraction and validated playlist entries.

use std::path::Path;

use serde::Deserialize;
use stem_split_audio::CancelToken;

use crate::{DownloadError, MediaUrl, Tool, install, process};

/// One downloadable item, with its title and validated source URL.
#[derive(Debug, Clone)]
pub struct MediaItem {
    title: String,
    url: MediaUrl,
}

impl MediaItem {
    /// Combines a validated URL and a display title, falling back to the URL.
    #[must_use]
    pub fn new(title: String, url: MediaUrl) -> Self {
        let title = if title.trim().is_empty() {
            url.as_str().to_owned()
        } else {
            title
        };
        Self { title, url }
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub const fn url(&self) -> &MediaUrl {
        &self.url
    }
}

/// A nonempty source. Playlist entries retain the source order.
#[derive(Debug, Clone)]
pub enum MediaInfo {
    Single(MediaItem),
    Playlist {
        title: String,
        first: MediaItem,
        rest: Vec<MediaItem>,
    },
}

impl MediaInfo {
    /// Returns a nonempty ordered subset, or None when nothing was selected.
    #[must_use]
    pub fn selected(&self, includes: impl Fn(usize) -> bool) -> Option<Self> {
        let mut items = self
            .items()
            .enumerate()
            .filter(|(index, _)| includes(*index))
            .map(|(_, item)| item.clone());
        let first = items.next()?;
        Some(match self {
            Self::Single(_) => Self::Single(first),
            Self::Playlist { title, .. } => Self::Playlist {
                title: title.clone(),
                first,
                rest: items.collect(),
            },
        })
    }

    #[must_use]
    pub fn title(&self) -> &str {
        match self {
            Self::Single(item) => item.title(),
            Self::Playlist { title, .. } => title,
        }
    }

    pub fn items(&self) -> impl Iterator<Item = &MediaItem> {
        let (first, rest) = match self {
            Self::Single(item) => (item, [].as_slice()),
            Self::Playlist { first, rest, .. } => (first, rest.as_slice()),
        };
        std::iter::once(first).chain(rest)
    }
}

/// Metadata extraction can be cancelled without failing.
#[derive(Debug)]
pub enum Inspected {
    Ready(MediaInfo),
    Cancelled,
}

#[derive(Deserialize)]
struct Metadata {
    #[serde(rename = "_type")]
    kind: Option<String>,
    title: Option<String>,
    url: Option<String>,
    webpage_url: Option<String>,
    is_live: Option<bool>,
    live_status: Option<String>,
    entries: Option<Vec<Option<Self>>>,
}

impl Metadata {
    fn item(self, fallback: &MediaUrl) -> Result<MediaItem, DownloadError> {
        if self.is_live == Some(true) || self.live_status.as_deref() == Some("is_live") {
            return Err(DownloadError::LiveStream);
        }
        let url = self.webpage_url.or(self.url).map_or_else(
            || Ok(fallback.clone()),
            |value| MediaUrl::new(&value).map_err(|_| DownloadError::InvalidEntry),
        )?;
        Ok(MediaItem::new(self.title.unwrap_or_default(), url))
    }
}

fn parse(bytes: &[u8], source: &MediaUrl) -> Result<MediaInfo, DownloadError> {
    let mut metadata: Metadata = serde_json::from_slice(bytes)?;
    if matches!(metadata.kind.as_deref(), Some("playlist" | "multi_video")) {
        let title = metadata
            .title
            .take()
            .unwrap_or_else(|| source.as_str().to_owned());
        let mut items = Vec::new();
        for entry in metadata.entries.take().into_iter().flatten().flatten() {
            // Flat entries must carry their own URL; never download the parent
            // playlist again for a missing or unavailable entry.
            if entry.webpage_url.is_none() && entry.url.is_none() {
                return Err(DownloadError::InvalidEntry);
            }
            items.push(entry.item(source)?);
        }
        let mut items = items.into_iter();
        let first = items.next().ok_or(DownloadError::EmptyPlaylist)?;
        Ok(MediaInfo::Playlist {
            title,
            first,
            rest: items.collect(),
        })
    } else {
        // A full metadata response's `url` may be an expiring media stream.
        // Keep the original page URL for the actual best-audio download.
        metadata.url = None;
        metadata.webpage_url = None;
        Ok(MediaInfo::Single(metadata.item(source)?))
    }
}

/// Fetches a title or a flat playlist without downloading media or `FFmpeg`.
/// The yt-dlp executable is installed in the model cache when needed.
///
/// # Errors
/// Returns installation, extraction, malformed-entry or unsupported-live errors.
pub fn inspect(
    url: &MediaUrl,
    cache: &Path,
    cancel: &CancelToken,
) -> Result<Inspected, DownloadError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        std::fs::create_dir_all(cache)?;
        let Some(youtube) = install(cache, Tool::Downloader, &|_| {}, cancel).await? else {
            return Ok(Inspected::Cancelled);
        };
        inspect_with(&youtube, url, cancel).await
    })
}

pub(super) async fn inspect_with(
    youtube: &Path,
    url: &MediaUrl,
    cancel: &CancelToken,
) -> Result<Inspected, DownloadError> {
    let args = [
        "--ignore-config",
        "--no-cache-dir",
        "--encoding",
        "utf-8",
        "--socket-timeout",
        "30",
        "--skip-download",
        "--flat-playlist",
        "--yes-playlist",
        "--dump-single-json",
        "--",
        url.as_str(),
    ];
    match process::capture(youtube, args, cancel).await? {
        Some(bytes) => Ok(Inspected::Ready(parse(&bytes, url)?)),
        None => Ok(Inspected::Cancelled),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_playlist_keeps_order_and_requires_downloadable_entries() -> Result<(), DownloadError> {
        let source =
            MediaUrl::new("https://example.org/list").map_err(|_| DownloadError::InvalidEntry)?;
        let info = parse(br#"{"_type":"playlist","title":"Album","entries":[{"title":"First","url":"https://example.org/1"},null,{"title":"Second","url":"https://example.org/2"}]}"#, &source)?;
        assert_eq!(info.title(), "Album");
        assert_eq!(
            info.items().map(MediaItem::title).collect::<Vec<_>>(),
            ["First", "Second"]
        );
        assert!(matches!(
            parse(br#"{"_type":"playlist","entries":[]}"#, &source),
            Err(DownloadError::EmptyPlaylist)
        ));
        assert!(matches!(
            parse(
                br#"{"_type":"playlist","entries":[{"title":"Unavailable"}]}"#,
                &source
            ),
            Err(DownloadError::InvalidEntry)
        ));
        assert!(matches!(
            parse(br#"{"is_live":true}"#, &source),
            Err(DownloadError::LiveStream)
        ));
        let single = parse(
            br#"{"title":"Track","url":"https://cdn.example.org/stream"}"#,
            &source,
        )?;
        assert_eq!(single.items().next().map(MediaItem::url), Some(&source));
        Ok(())
    }
}
