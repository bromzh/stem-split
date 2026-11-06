//! Inspects media URLs and downloads audio into a chosen folder off the UI thread.
//!
//! yt-dlp provides binary installation. Child processes are owned here so
//! cancellation can kill and reap them, and playlists can be expanded before
//! downloading. All intermediate files have RAII owners.

mod encoding;
mod metadata;
pub use encoding::{FfmpegEncoder, SaveFormat};
mod process;

pub use metadata::{Inspected, MediaInfo, MediaItem, inspect};

use std::{
    fs, io,
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use stem_split_core::{
    CancelToken,
    domain::{InputFile, InputFileError, OutputDir},
};
use yt_dlp::client::deps::LibraryInstaller;

/// An HTTP(S) media or playlist URL, validated at the UI boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaUrl(url::Url);

/// Why a URL cannot be used as a media source.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UrlError {
    #[error("enter an absolute HTTP or HTTPS URL without embedded credentials")]
    Invalid,
}

impl MediaUrl {
    /// Parses a URL without accessing the network.
    ///
    /// # Errors
    /// Returns [`UrlError::Invalid`] for unsupported or malformed URLs.
    pub fn new(value: &str) -> Result<Self, UrlError> {
        let parsed = url::Url::parse(value.trim()).map_err(|_| UrlError::Invalid)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(UrlError::Invalid);
        }
        Ok(Self(parsed))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// A stage shown in the existing progress screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    InstallingDownloader,
    InstallingFfmpeg,
    Inspecting,
    Downloading {
        downloaded: u64,
        total: Option<NonZeroU64>,
    },
    Converting,
}

/// Cancellation is a normal outcome, not an error.
#[derive(Debug)]
pub enum Downloaded {
    Ready(Vec<DownloadedAudio>),
    Cancelled,
}

/// A native source file and its readable inference input.
/// Private temporary decoding files live only as long as this owner.
#[derive(Debug)]
pub struct DownloadedAudio {
    source: PathBuf,
    input: InputFile,
    _decoded: Option<Arc<DecodedDirectory>>,
}

impl DownloadedAudio {
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }
    #[must_use]
    pub const fn input(&self) -> &InputFile {
        &self.input
    }
}

#[derive(Debug)]
struct DecodedDirectory {
    path: PathBuf,
    directory: Option<tempfile::TempDir>,
}

impl Drop for DecodedDirectory {
    fn drop(&mut self) {
        let Some(directory) = self.directory.take() else {
            return;
        };
        // The last reference can be released by a UI edit. Disable synchronous
        // TempDir cleanup and delete the shared batch directory off-thread.
        let path = directory.keep();
        match std::thread::Builder::new()
            .name("decoded audio cleanup".to_owned())
            .spawn(move || {
                if let Err(error) = fs::remove_dir_all(path) {
                    log::warn!("cannot remove decoded audio: {error}");
                }
            }) {
            Ok(_) => {}
            Err(error) => log::warn!("cannot start decoded audio cleanup: {error}"),
        }
    }
}

/// Download events are always associated with a selected item.
#[derive(Debug)]
pub enum DownloadEvent {
    Progress(usize, Progress),
    Finished(usize, Result<PathBuf, DownloadError>),
}

/// Why the source audio could not be prepared.
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("media I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("cannot install a media tool: {0}")]
    Tool(#[from] yt_dlp::error::Error),
    #[error("media process failed: {0}")]
    Process(String),
    #[error("cannot read media metadata: {0}")]
    Metadata(#[from] serde_json::Error),
    #[error("live streams are not supported")]
    LiveStream,
    #[error("the playlist contains no media entries")]
    EmptyPlaylist,
    #[error("a playlist entry has no supported HTTP(S) URL")]
    InvalidEntry,
    #[error("downloaded audio is invalid: {0}")]
    Input(#[from] InputFileError),
    #[error("cannot publish downloaded audio: {0}")]
    Publish(#[from] tempfile::PathPersistError),
}

/// Downloads native audio, reporting and skipping individual item failures.
/// `cache` is the same flat directory used by the model store.
///
/// # Errors
/// Returns a typed error for installation, extraction, conversion or file I/O
/// failures. Cancellation returns [`Downloaded::Cancelled`].
pub fn download(
    info: &MediaInfo,
    folder: &OutputDir,
    cache: &Path,
    format: SaveFormat,
    report: impl Fn(DownloadEvent),
    cancel: &CancelToken,
) -> Result<Downloaded, DownloadError> {
    if cancel.is_cancelled() {
        return Ok(Downloaded::Cancelled);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        // The future is dropped only after owned children have been reaped.
        prepare(info, folder, cache, format, &report, cancel).await
    })
}

async fn prepare(
    info: &MediaInfo,
    folder: &OutputDir,
    cache: &Path,
    format: SaveFormat,
    report: &impl Fn(DownloadEvent),
    cancel: &CancelToken,
) -> Result<Downloaded, DownloadError> {
    fs::create_dir_all(cache)?;
    let Some(youtube) = install(
        cache,
        Tool::Downloader,
        &|progress| report(DownloadEvent::Progress(0, progress)),
        cancel,
    )
    .await?
    else {
        return Ok(Downloaded::Cancelled);
    };
    let directory = tempfile::Builder::new()
        .prefix(".media-input-")
        .tempdir_in(cache)?;
    let decoded = Arc::new(DecodedDirectory {
        path: directory.path().to_path_buf(),
        directory: Some(directory),
    });
    let preparation = Preparation {
        youtube: youtube.as_path(),
        folder,
        cache,
        decoded: &decoded,
        format,
    };
    let preparation = &preparation;
    let inputs = download_each(info, report, cancel, |index, item| async move {
        prepare_item(
            item,
            preparation,
            &|progress| report(DownloadEvent::Progress(index, progress)),
            cancel,
        )
        .await
    })
    .await;
    Ok(Downloaded::Ready(inputs))
}

async fn download_each<'a, F, Fut>(
    info: &'a MediaInfo,
    report: &impl Fn(DownloadEvent),
    cancel: &CancelToken,
    mut prepare: F,
) -> Vec<DownloadedAudio>
where
    F: FnMut(usize, &'a MediaItem) -> Fut,
    Fut: std::future::Future<Output = Result<Option<DownloadedAudio>, DownloadError>>,
{
    let mut inputs = Vec::new();
    for (index, item) in info.items().enumerate() {
        if cancel.is_cancelled() {
            break;
        }
        match prepare(index, item).await {
            Ok(Some(audio)) => {
                report(DownloadEvent::Finished(index, Ok(audio.source.clone())));
                inputs.push(audio);
            }
            Ok(None) => break,
            Err(error) => report(DownloadEvent::Finished(index, Err(error))),
        }
    }
    inputs
}

struct Preparation<'a> {
    youtube: &'a Path,
    folder: &'a OutputDir,
    cache: &'a Path,
    decoded: &'a Arc<DecodedDirectory>,
    format: SaveFormat,
}

async fn prepare_item(
    item: &MediaItem,
    preparation: &Preparation<'_>,
    report: &impl Fn(Progress),
    cancel: &CancelToken,
) -> Result<Option<DownloadedAudio>, DownloadError> {
    let Preparation {
        youtube,
        folder,
        cache,
        decoded,
        format,
    } = preparation;
    report(Progress::Inspecting);
    let Inspected::Ready(MediaInfo::Single(_)) =
        metadata::inspect_with(youtube, item.url(), cancel).await?
    else {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        return Err(DownloadError::InvalidEntry);
    };
    let stage = tempfile::Builder::new()
        .prefix(".stem-split-download-")
        .tempdir_in(folder.path())?;
    report(Progress::Downloading {
        downloaded: 0,
        total: None,
    });
    if !process::audio(youtube, item.url(), stage.path(), report, cancel).await? {
        return Ok(None);
    }
    let source = fs::read_dir(stage.path())?
        .collect::<Result<Vec<_>, io::Error>>()?
        .into_iter()
        .map(|entry| entry.path())
        .find(|path| path.file_stem().is_some_and(|stem| stem == "source"))
        .ok_or_else(|| DownloadError::Process("yt-dlp did not produce an audio file".to_owned()))?;
    if cancel.is_cancelled() {
        return Ok(None);
    }
    let native = source;
    let source = match format.encoding() {
        None => native.clone(),
        Some(encoding) => {
            let Some(ffmpeg) = install(cache, Tool::Ffmpeg, report, cancel).await? else {
                return Ok(None);
            };
            report(Progress::Converting);
            let encoded = stage.path().join(format!("saved.{}", encoding.extension()));
            if !process::encode(&ffmpeg, &native, &encoded, encoding, cancel).await? {
                return Ok(None);
            }
            encoded
        }
    };
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .ok_or(DownloadError::InvalidEntry)?;
    let path = publish_source(&source, folder, item.title())?;
    // Lossy saved copies must not become the input to separation. Use the
    // original stream for private decoding; original/lossless files can be read directly.
    if *format != SaveFormat::Mp3
        && matches!(extension, "mp3" | "flac" | "wav" | "m4a" | "mp4")
        && let Ok(input) = InputFile::new(&path)
    {
        return Ok(Some(DownloadedAudio {
            source: path,
            input,
            _decoded: None,
        }));
    }
    let Some(ffmpeg) = install(cache, Tool::Ffmpeg, report, cancel).await? else {
        return Ok(None);
    };
    report(Progress::Converting);
    let mut filename = path
        .file_stem()
        .ok_or(DownloadError::InvalidEntry)?
        .to_os_string();
    filename.push(".wav");
    let output = decoded.path.join(filename);
    let inference_source = if *format == SaveFormat::Mp3 {
        native.as_path()
    } else {
        path.as_path()
    };
    if !process::convert(&ffmpeg, inference_source, &output, cancel).await? {
        return Ok(None);
    }
    let input = InputFile::new(&output)?;
    Ok(Some(DownloadedAudio {
        source: path,
        input,
        _decoded: Some(Arc::clone(decoded)),
    }))
}

fn publish_source(
    source: &Path,
    folder: &OutputDir,
    title: &str,
) -> Result<PathBuf, DownloadError> {
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .ok_or(DownloadError::InvalidEntry)?;
    let title = safe_title(title);
    let published = tempfile::Builder::new()
        .prefix(&format!("{title}-"))
        .suffix(&format!(".{extension}"))
        .tempfile_in(folder.path())?
        .into_temp_path();
    fs::rename(source, &published)?;
    Ok(published.keep()?)
}

fn safe_title(title: &str) -> String {
    let title: String = title
        .chars()
        .take(80)
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '-' | '_' | ' ') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let title = title.trim();
    if title.is_empty() {
        "audio".to_owned()
    } else {
        title.to_owned()
    }
}

#[derive(Clone, Copy)]
enum Tool {
    Downloader,
    Ffmpeg,
}

impl Tool {
    const fn name(self) -> &'static str {
        match self {
            Self::Downloader => "yt-dlp",
            Self::Ffmpeg => "ffmpeg",
        }
    }
    const fn progress(self) -> Progress {
        match self {
            Self::Downloader => Progress::InstallingDownloader,
            Self::Ffmpeg => Progress::InstallingFfmpeg,
        }
    }
}

async fn install(
    cache: &Path,
    tool: Tool,
    report: &impl Fn(Progress),
    cancel: &CancelToken,
) -> Result<Option<PathBuf>, DownloadError> {
    let path = cache.join(format!("{}{}", tool.name(), std::env::consts::EXE_SUFFIX));
    if path.is_file() {
        return Ok(Some(path));
    }
    if cancel.is_cancelled() {
        return Ok(None);
    }
    report(tool.progress());
    // Publish only a completed binary; interrupted installations cannot be
    // mistaken for cached tools on the next run.
    let stage = tempfile::Builder::new()
        .prefix(".media-tools-")
        .tempdir_in(cache)?;
    let installer = LibraryInstaller::new(stage.path().to_path_buf());
    let install = async {
        match tool {
            Tool::Downloader => installer.install_youtube(None).await,
            Tool::Ffmpeg => installer.install_ffmpeg(None).await,
        }
    };
    tokio::pin!(install);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let binary = loop {
        tokio::select! {
            result = &mut install => break result?,
            _ = tick.tick() => if cancel.is_cancelled() { return Ok(None); },
        }
    };
    if let Err(error) = fs::rename(binary, &path) {
        // A cancelled preview may finish installation while a download starts.
        // Both install into private staging directories; accept a published tool.
        if !path.is_file() {
            return Err(error.into());
        }
    }
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_download_publication_preserves_extension_and_bytes() -> Result<(), DownloadError> {
        let directory = tempfile::tempdir()?;
        let folder = OutputDir::new(directory.path())
            .map_err(|error| io::Error::other(error.to_string()))?;
        let source = directory.path().join("source.webm");
        fs::write(&source, b"native codec bytes")?;
        let published = publish_source(&source, &folder, "Track")?;
        assert_eq!(published.extension(), Some(std::ffi::OsStr::new("webm")));
        assert_eq!(fs::read(published)?, b"native codec bytes");
        Ok(())
    }

    #[test]
    fn an_item_failure_does_not_prevent_later_downloads() -> Result<(), DownloadError> {
        let directory = tempfile::tempdir()?;
        let url =
            MediaUrl::new("https://example.org/track").map_err(|_| DownloadError::InvalidEntry)?;
        let info = MediaInfo::Playlist {
            title: "List".to_owned(),
            first: MediaItem::new("First".to_owned(), url.clone()),
            rest: vec![
                MediaItem::new("Fails".to_owned(), url.clone()),
                MediaItem::new("Last".to_owned(), url),
            ],
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let report = |event| match sender.send(event) {
            Ok(()) | Err(_) => {}
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let downloaded = runtime.block_on(download_each(
            &info,
            &report,
            &CancelToken::new(),
            |index, _| {
                let result = if index == 1 {
                    Err(DownloadError::InvalidEntry)
                } else {
                    let source = directory.path().join(format!("track-{index}.mp3"));
                    fs::write(&source, b"")
                        .map_err(DownloadError::Io)
                        .and_then(|()| {
                            Ok(Some(DownloadedAudio {
                                input: InputFile::new(&source)?,
                                source,
                                _decoded: None,
                            }))
                        })
                };
                std::future::ready(result)
            },
        ));
        assert_eq!(downloaded.len(), 2);
        let events: Vec<_> = receiver.try_iter().collect();
        assert!(matches!(
            events.as_slice(),
            [
                DownloadEvent::Finished(0, Ok(_)),
                DownloadEvent::Finished(1, Err(_)),
                DownloadEvent::Finished(2, Ok(_))
            ]
        ));
        Ok(())
    }

    #[test]
    fn urls_require_http_and_no_embedded_credentials() -> Result<(), UrlError> {
        assert_eq!(
            MediaUrl::new(" https://example.org/audio ")?.as_str(),
            "https://example.org/audio"
        );
        for value in [
            "",
            "not a url",
            "file:///tmp/audio.wav",
            "ftp://example.org/audio",
            "https://user:pass@example.org/audio",
        ] {
            assert!(MediaUrl::new(value).is_err());
        }
        Ok(())
    }
}
