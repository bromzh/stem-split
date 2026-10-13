use std::{
    fs,
    io::{self, Read, Write},
    iter,
    path::{Path, PathBuf},
    time::Duration,
};

use directories::ProjectDirs;
use sha2::{Digest, Sha256};
use ureq::{
    Agent,
    tls::{RootCerts, TlsConfig},
};

use super::registry::{BASE_URL, GraphSpec, ModelSpec, StemSelection};
use crate::{
    cancel::CancelToken,
    domain::{ByteCount, ModelId, OverwritePolicy, Sha256Digest},
    partial::{PartialFile, PartialFileError},
};

/// Name the per-user directories are created under.
const APPLICATION: &str = "stem-split";
/// Subdirectory of the cache that holds model files.
const MODELS_DIRECTORY: &str = "models";
/// How much is read from the network between two cancellation checks.
const BLOCK_BYTES: usize = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const RESPONSE_TIMEOUT: Duration = Duration::from_mins(1);

/// Why a model could not be made available locally.
#[derive(Debug, thiserror::Error)]
pub enum ModelStoreError {
    #[error("no cache directory is available for the current user")]
    NoCacheDirectory,
    #[error("cannot create the model directory `{}`: {source}", path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot download `{url}`: {source}")]
    Request {
        url: String,
        #[source]
        source: Box<ureq::Error>,
    },
    #[error("the download of `{url}` was interrupted: {source}")]
    Transfer {
        url: String,
        #[source]
        source: io::Error,
    },
    #[error("cannot write `{}`: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("`{url}` has {} bytes, expected {}", actual.get(), expected.get())]
    WrongSize {
        url: String,
        expected: ByteCount,
        actual: ByteCount,
    },
    #[error("`{url}` has SHA-256 digest {actual}, expected {expected}")]
    DigestMismatch {
        url: String,
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    #[error("cannot move the downloaded file to `{}`: {source}", path.display())]
    Publish {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl From<PartialFileError> for ModelStoreError {
    fn from(error: PartialFileError) -> Self {
        match error {
            PartialFileError::Create { path, source } => Self::Write { path, source },
            PartialFileError::Publish { path, source } => Self::Publish { path, source },
            // Neither can happen here: the store replaces files, and builds
            // every path from a file name. They are mapped all the same so
            // that a change in `PartialFile` cannot go unhandled.
            PartialFileError::TargetExists { path } => Self::Write {
                source: io::Error::new(io::ErrorKind::AlreadyExists, "file already exists"),
                path,
            },
            PartialFileError::InvalidTarget { path } => Self::Write {
                source: io::Error::new(io::ErrorKind::InvalidInput, "path does not name a file"),
                path,
            },
        }
    }
}

/// How far a download has come. Counts cover every file of the model that
/// had to be fetched, not just the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    downloaded: ByteCount,
    total: ByteCount,
}

impl DownloadProgress {
    #[must_use]
    pub const fn downloaded(self) -> ByteCount {
        self.downloaded
    }

    #[must_use]
    pub const fn total(self) -> ByteCount {
        self.total
    }
}

/// A graph file on the local disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalGraph {
    path: PathBuf,
    emits: StemSelection,
}

impl LocalGraph {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn emits(&self) -> StemSelection {
        self.emits
    }
}

/// A model whose files are all on the local disk. Never empty, like the
/// [`ModelSpec`] it was made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalModel {
    first: LocalGraph,
    rest: Vec<LocalGraph>,
}

impl LocalModel {
    /// Graphs in the order they are run.
    pub fn graphs(&self) -> impl Iterator<Item = &LocalGraph> {
        iter::once(&self.first).chain(&self.rest)
    }
}

/// How [`ModelStore::ensure`] ended, short of an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ensured {
    Ready(LocalModel),
    /// The user cancelled while files were being downloaded. Files that had
    /// finished stay in the cache; the unfinished one is removed.
    Cancelled,
}

/// Whether the files asked for are all in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fetch {
    Complete,
    Cancelled,
}

/// Whether plain HTTP is acceptable. Only tests, which talk to a server on
/// the loopback interface, need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    HttpsOnly,
    #[cfg(test)]
    AllowPlainHttp,
}

/// Model files on the local disk, downloaded on first use.
///
/// A file appears in the store only after its size and SHA-256 digest have
/// been checked, under a name derived from that digest. Finding a file with
/// the right name and size is therefore enough to trust it later.
#[derive(Debug)]
pub struct ModelStore {
    directory: PathBuf,
    base_url: String,
    agent: Agent,
}

impl ModelStore {
    /// A store that keeps its files in `directory`.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self::with_source(directory.into(), BASE_URL, Transport::HttpsOnly)
    }

    /// A store in the current user's cache directory.
    ///
    /// # Errors
    ///
    /// Returns [`ModelStoreError::NoCacheDirectory`] if the operating system
    /// reports no home directory for the user.
    pub fn in_user_cache() -> Result<Self, ModelStoreError> {
        let directories =
            ProjectDirs::from("", "", APPLICATION).ok_or(ModelStoreError::NoCacheDirectory)?;
        Ok(Self::new(directories.cache_dir().join(MODELS_DIRECTORY)))
    }

    fn with_source(directory: PathBuf, base_url: &str, transport: Transport) -> Self {
        let https_only = match transport {
            Transport::HttpsOnly => true,
            #[cfg(test)]
            Transport::AllowPlainHttp => false,
        };
        let agent = Agent::config_builder()
            .https_only(https_only)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(RESPONSE_TIMEOUT))
            .user_agent(concat!("stem-split/", env!("CARGO_PKG_VERSION")))
            // The system's own trust store, so that certificates installed by
            // an administrator or a security product are honoured.
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .new_agent();
        Self {
            directory,
            base_url: base_url.to_owned(),
            agent,
        }
    }

    /// The directory the files are kept in.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Whether every file of `model` is already in the store, so that
    /// [`ModelStore::ensure`] would not touch the network.
    #[must_use]
    pub fn is_cached(&self, model: ModelId) -> bool {
        model.spec().graphs().all(|graph| self.has(graph))
    }

    /// Makes sure every file of `model` is in the store, downloading the
    /// missing ones.
    ///
    /// `on_progress` is called after each block received from the network;
    /// it is not called at all when nothing has to be downloaded. `cancel`
    /// is checked between blocks.
    ///
    /// # Errors
    ///
    /// Returns an error if the store directory cannot be created, a file
    /// cannot be downloaded or written, or a downloaded file does not have
    /// the expected size and digest. A file that fails is not kept.
    pub fn ensure(
        &self,
        model: ModelId,
        on_progress: &mut dyn FnMut(DownloadProgress),
        cancel: &CancelToken,
    ) -> Result<Ensured, ModelStoreError> {
        let spec = model.spec();
        let graphs: Vec<&GraphSpec> = spec.graphs().collect();
        match self.fetch_missing(&graphs, on_progress, cancel)? {
            Fetch::Complete => Ok(Ensured::Ready(self.locate(spec))),
            Fetch::Cancelled => Ok(Ensured::Cancelled),
        }
    }

    fn locate(&self, spec: &ModelSpec) -> LocalModel {
        let local = |graph: &GraphSpec| LocalGraph {
            path: self.path_of(graph),
            emits: graph.emits(),
        };
        LocalModel {
            first: local(spec.first()),
            rest: spec.rest().iter().map(local).collect(),
        }
    }

    fn path_of(&self, graph: &GraphSpec) -> PathBuf {
        self.directory.join(graph.cache_file_name())
    }

    fn has(&self, graph: &GraphSpec) -> bool {
        fs::metadata(self.path_of(graph))
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == graph.size().get())
    }

    fn fetch_missing(
        &self,
        graphs: &[&GraphSpec],
        on_progress: &mut dyn FnMut(DownloadProgress),
        cancel: &CancelToken,
    ) -> Result<Fetch, ModelStoreError> {
        let missing: Vec<&GraphSpec> = graphs
            .iter()
            .copied()
            .filter(|graph| !self.has(graph))
            .collect();
        if missing.is_empty() {
            return Ok(Fetch::Complete);
        }
        fs::create_dir_all(&self.directory).map_err(|source| ModelStoreError::CreateDirectory {
            path: self.directory.clone(),
            source,
        })?;

        let total = ByteCount::new(
            missing
                .iter()
                .fold(0, |total, graph| total.saturating_add(graph.size().get())),
        );
        let mut downloaded = 0_u64;
        for graph in missing {
            let mut on_block = |bytes: u64| {
                downloaded = downloaded.saturating_add(bytes);
                on_progress(DownloadProgress {
                    downloaded: ByteCount::new(downloaded),
                    total,
                });
            };
            match self.download(graph, &mut on_block, cancel)? {
                Fetch::Complete => {}
                Fetch::Cancelled => return Ok(Fetch::Cancelled),
            }
        }
        Ok(Fetch::Complete)
    }

    /// Downloads one graph into the store, verifying it before it becomes
    /// visible under its final name.
    fn download(
        &self,
        graph: &GraphSpec,
        on_block: &mut dyn FnMut(u64),
        cancel: &CancelToken,
    ) -> Result<Fetch, ModelStoreError> {
        let url = format!("{}/{}", self.base_url, graph.remote_file_name());
        let target = self.path_of(graph);
        let expected = graph.size().get();

        // Dropping `partial` on any early return below removes the file.
        let (partial, mut file) = PartialFile::create(&target, OverwritePolicy::Replace)?;
        let mut response =
            self.agent
                .get(url.as_str())
                .call()
                .map_err(|source| ModelStoreError::Request {
                    url: url.clone(),
                    source: Box::new(source),
                })?;
        let mut reader = response.body_mut().as_reader();

        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; BLOCK_BYTES];
        let mut received = 0_u64;
        loop {
            if cancel.is_cancelled() {
                return Ok(Fetch::Cancelled);
            }
            let block = read_block(&mut reader, &mut buffer).map_err(|source| {
                ModelStoreError::Transfer {
                    url: url.clone(),
                    source,
                }
            })?;
            if block.is_empty() {
                break;
            }
            let length = u64::try_from(block.len()).unwrap_or(u64::MAX);
            received = received.saturating_add(length);
            // Stop as soon as the server sends more than the file can hold,
            // instead of filling the disk first.
            if received > expected {
                return Err(ModelStoreError::WrongSize {
                    url,
                    expected: graph.size(),
                    actual: ByteCount::new(received),
                });
            }
            hasher.update(block);
            file.write_all(block)
                .map_err(|source| ModelStoreError::Write {
                    path: target.clone(),
                    source,
                })?;
            on_block(length);
        }

        if received != expected {
            return Err(ModelStoreError::WrongSize {
                url,
                expected: graph.size(),
                actual: ByteCount::new(received),
            });
        }
        let actual = Sha256Digest::new(hasher.finalize().into());
        if actual != graph.sha256() {
            return Err(ModelStoreError::DigestMismatch {
                url,
                expected: graph.sha256(),
                actual,
            });
        }
        file.flush().map_err(|source| ModelStoreError::Write {
            path: target.clone(),
            source,
        })?;
        drop(file);
        partial.publish()?;
        Ok(Fetch::Complete)
    }
}

/// Reads the next block into `buffer`, retrying reads interrupted by a
/// signal. An empty block means the end of the stream.
fn read_block<'a>(reader: &mut impl Read, buffer: &'a mut [u8]) -> io::Result<&'a [u8]> {
    let count = loop {
        match reader.read(buffer) {
            Ok(count) => break count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    };
    buffer.get(..count).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "reader reported more bytes than the buffer holds",
        )
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        error::Error,
        io::{BufRead, BufReader},
        net::{TcpListener, TcpStream},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    use super::*;

    /// What the test server answers for a path.
    #[derive(Debug, Clone)]
    enum Route {
        Body(Vec<u8>),
        /// Announces `declared` bytes, sends fewer and closes the connection.
        Truncated {
            declared: usize,
            sent: Vec<u8>,
        },
        Status(u16),
    }

    /// A minimal HTTP server on the loopback interface. It serves each
    /// connection one response and runs until the test process ends.
    struct TestServer {
        base_url: String,
        requests: Arc<AtomicUsize>,
    }

    impl TestServer {
        fn start(routes: HashMap<String, Route>) -> io::Result<Self> {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let base_url = format!("http://{}", listener.local_addr()?);
            let requests = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&requests);
            thread::Builder::new()
                .name("test-http-server".to_owned())
                .spawn(move || {
                    for stream in listener.incoming() {
                        let Ok(stream) = stream else { return };
                        counter.fetch_add(1, Ordering::SeqCst);
                        // A client that hangs up early only ends its own connection.
                        match respond(stream, &routes) {
                            Ok(()) | Err(_) => {}
                        }
                    }
                })?;
            Ok(Self { base_url, requests })
        }

        fn requests(&self) -> usize {
            self.requests.load(Ordering::SeqCst)
        }

        fn store(&self, directory: &Path) -> ModelStore {
            ModelStore::with_source(
                directory.to_path_buf(),
                &self.base_url,
                Transport::AllowPlainHttp,
            )
        }
    }

    fn respond(mut stream: TcpStream, routes: &HashMap<String, Route>) -> io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header)? == 0 || header == "\r\n" {
                break;
            }
        }
        let path = request_line.split_whitespace().nth(1).unwrap_or("/");

        let (status, declared, body): (u16, usize, &[u8]) = match routes.get(path) {
            Some(Route::Body(body)) => (200, body.len(), body),
            Some(Route::Truncated { declared, sent }) => (200, *declared, sent),
            Some(Route::Status(status)) => (*status, 0, &[]),
            None => (404, 0, &[]),
        };
        write!(
            stream,
            "HTTP/1.1 {status} Test\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n"
        )?;
        stream.write_all(body)?;
        stream.flush()
    }

    /// A graph spec that describes `content` exactly.
    fn spec_for(name: &'static str, content: &[u8]) -> GraphSpec {
        GraphSpec::new(
            name,
            ByteCount::new(u64::try_from(content.len()).unwrap_or(u64::MAX)),
            Sha256Digest::new(Sha256::digest(content).into()),
            StemSelection::All,
        )
    }

    fn route(graph: &GraphSpec) -> String {
        format!("/{}", graph.remote_file_name())
    }

    /// Deterministic filler that is not all one byte.
    fn payload(length: usize) -> Vec<u8> {
        (0..length)
            .map(|index| {
                let [low, ..] = index.to_le_bytes();
                low
            })
            .collect()
    }

    fn entries(directory: &Path) -> io::Result<Vec<String>> {
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut names = Vec::new();
        for entry in fs::read_dir(directory)? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }

    #[test]
    fn file_is_downloaded_once_and_then_served_from_the_cache() -> Result<(), Box<dyn Error>> {
        let content = payload(200_000);
        let graph = spec_for("tiny", &content);
        let server = TestServer::start(HashMap::from([(
            route(&graph),
            Route::Body(content.clone()),
        )]))?;
        let directory = tempfile::tempdir()?;
        let store = server.store(&directory.path().join("models"));
        let cancel = CancelToken::new();

        let mut reports = Vec::new();
        let first =
            store.fetch_missing(&[&graph], &mut |progress| reports.push(progress), &cancel)?;

        assert_eq!(first, Fetch::Complete);
        assert_eq!(fs::read(store.path_of(&graph))?, content);
        assert_eq!(entries(store.directory())?, [graph.cache_file_name()]);
        assert_eq!(server.requests(), 1);
        // Progress grows monotonically and ends at the full size.
        assert!(reports.len() >= 2, "one report per block");
        assert!(reports.is_sorted_by_key(|progress| progress.downloaded()));
        assert_eq!(
            reports.last().map(|progress| progress.downloaded()),
            Some(graph.size())
        );
        assert!(
            reports
                .iter()
                .all(|progress| progress.total() == graph.size())
        );

        let mut later_reports = 0;
        let second = store.fetch_missing(&[&graph], &mut |_| later_reports += 1, &cancel)?;

        assert_eq!(second, Fetch::Complete);
        assert_eq!(
            server.requests(),
            1,
            "the cached file must not be requested again"
        );
        assert_eq!(later_reports, 0);
        Ok(())
    }

    #[test]
    fn only_missing_files_are_downloaded_and_counted() -> Result<(), Box<dyn Error>> {
        let cached = payload(1_000);
        let missing = payload(3_000);
        let cached_graph = spec_for("cached", &cached);
        let missing_graph = spec_for("missing", &missing);
        let server = TestServer::start(HashMap::from([
            (route(&cached_graph), Route::Body(cached.clone())),
            (route(&missing_graph), Route::Body(missing.clone())),
        ]))?;
        let directory = tempfile::tempdir()?;
        let store = server.store(directory.path());
        fs::write(store.path_of(&cached_graph), &cached)?;

        let mut last = None;
        let result = store.fetch_missing(
            &[&cached_graph, &missing_graph],
            &mut |progress| last = Some(progress),
            &CancelToken::new(),
        )?;

        assert_eq!(result, Fetch::Complete);
        assert_eq!(server.requests(), 1);
        assert_eq!(fs::read(store.path_of(&missing_graph))?, missing);
        let last = last.ok_or("no progress was reported")?;
        assert_eq!(last.total(), missing_graph.size());
        assert_eq!(last.downloaded(), missing_graph.size());
        Ok(())
    }

    #[test]
    fn corrupted_download_is_rejected_and_not_kept() -> Result<(), Box<dyn Error>> {
        let content = payload(50_000);
        let graph = spec_for("tiny", &content);
        let mut damaged = content.clone();
        if let Some(byte) = damaged.get_mut(12_345) {
            *byte ^= 0xFF;
        }
        let server = TestServer::start(HashMap::from([(route(&graph), Route::Body(damaged))]))?;
        let directory = tempfile::tempdir()?;
        let store = server.store(directory.path());

        let result = store.fetch_missing(&[&graph], &mut |_| {}, &CancelToken::new());

        assert!(matches!(
            result,
            Err(ModelStoreError::DigestMismatch { expected, actual, .. })
                if expected == graph.sha256() && actual != expected
        ));
        assert_eq!(entries(store.directory())?, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn download_of_the_wrong_size_is_rejected_and_not_kept() -> Result<(), Box<dyn Error>> {
        let content = payload(50_000);
        let graph = spec_for("tiny", &content);
        let directory = tempfile::tempdir()?;

        for served in [payload(49_999), payload(200_000)] {
            let actual_length = served.len();
            let server = TestServer::start(HashMap::from([(route(&graph), Route::Body(served))]))?;
            let store = server.store(directory.path());

            let result = store.fetch_missing(&[&graph], &mut |_| {}, &CancelToken::new());

            assert!(
                matches!(
                    result,
                    Err(ModelStoreError::WrongSize { expected, .. }) if expected == graph.size()
                ),
                "served {actual_length} bytes"
            );
            assert_eq!(entries(store.directory())?, Vec::<String>::new());
        }
        Ok(())
    }

    #[test]
    fn connection_closed_midway_is_an_error_and_leaves_nothing() -> Result<(), Box<dyn Error>> {
        let content = payload(100_000);
        let graph = spec_for("tiny", &content);
        let server = TestServer::start(HashMap::from([(
            route(&graph),
            Route::Truncated {
                declared: content.len(),
                sent: payload(30_000),
            },
        )]))?;
        let directory = tempfile::tempdir()?;
        let store = server.store(directory.path());

        let result = store.fetch_missing(&[&graph], &mut |_| {}, &CancelToken::new());

        assert!(matches!(result, Err(ModelStoreError::Transfer { .. })));
        assert_eq!(entries(store.directory())?, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn missing_file_on_the_server_is_an_error() -> Result<(), Box<dyn Error>> {
        let graph = spec_for("tiny", b"content");
        let server = TestServer::start(HashMap::from([(route(&graph), Route::Status(404))]))?;
        let directory = tempfile::tempdir()?;
        let store = server.store(directory.path());

        let result = store.fetch_missing(&[&graph], &mut |_| {}, &CancelToken::new());

        assert!(matches!(
            result,
            Err(ModelStoreError::Request { source, .. })
                if matches!(*source, ureq::Error::StatusCode(404))
        ));
        assert_eq!(entries(store.directory())?, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn cancelling_during_a_download_leaves_no_files() -> Result<(), Box<dyn Error>> {
        let content = payload(BLOCK_BYTES * 6);
        let graph = spec_for("tiny", &content);
        let server = TestServer::start(HashMap::from([(route(&graph), Route::Body(content))]))?;
        let directory = tempfile::tempdir()?;
        let store = server.store(directory.path());
        let cancel = CancelToken::new();

        let mut reports = 0;
        let result = store.fetch_missing(
            &[&graph],
            &mut |_| {
                reports += 1;
                cancel.cancel();
            },
            &cancel,
        )?;

        assert_eq!(result, Fetch::Cancelled);
        assert_eq!(reports, 1, "no block is processed after cancellation");
        assert_eq!(entries(store.directory())?, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn cancelling_keeps_files_that_already_finished() -> Result<(), Box<dyn Error>> {
        let first = payload(1_000);
        let second = payload(BLOCK_BYTES * 4);
        let first_graph = spec_for("first", &first);
        let second_graph = spec_for("second", &second);
        let server = TestServer::start(HashMap::from([
            (route(&first_graph), Route::Body(first)),
            (route(&second_graph), Route::Body(second)),
        ]))?;
        let directory = tempfile::tempdir()?;
        let store = server.store(directory.path());
        let cancel = CancelToken::new();

        let result = store.fetch_missing(
            &[&first_graph, &second_graph],
            &mut |progress| {
                if progress.downloaded() > first_graph.size() {
                    cancel.cancel();
                }
            },
            &cancel,
        )?;

        assert_eq!(result, Fetch::Cancelled);
        assert_eq!(entries(store.directory())?, [first_graph.cache_file_name()]);
        Ok(())
    }

    #[test]
    fn cached_file_of_the_wrong_size_is_downloaded_again() -> Result<(), Box<dyn Error>> {
        let content = payload(5_000);
        let graph = spec_for("tiny", &content);
        let server = TestServer::start(HashMap::from([(
            route(&graph),
            Route::Body(content.clone()),
        )]))?;
        let directory = tempfile::tempdir()?;
        let store = server.store(directory.path());
        fs::write(store.path_of(&graph), b"cut short")?;

        let result = store.fetch_missing(&[&graph], &mut |_| {}, &CancelToken::new())?;

        assert_eq!(result, Fetch::Complete);
        assert_eq!(server.requests(), 1);
        assert_eq!(fs::read(store.path_of(&graph))?, content);
        Ok(())
    }

    #[test]
    fn plain_http_is_refused_outside_tests() -> Result<(), Box<dyn Error>> {
        let content = payload(100);
        let graph = spec_for("tiny", &content);
        let server = TestServer::start(HashMap::from([(route(&graph), Route::Body(content))]))?;
        let directory = tempfile::tempdir()?;
        let store = ModelStore::with_source(
            directory.path().to_path_buf(),
            &server.base_url,
            Transport::HttpsOnly,
        );

        let result = store.fetch_missing(&[&graph], &mut |_| {}, &CancelToken::new());

        assert!(matches!(result, Err(ModelStoreError::Request { .. })));
        assert_eq!(server.requests(), 0);
        assert_eq!(entries(store.directory())?, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn empty_store_reports_models_as_not_cached() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let store = ModelStore::new(directory.path().join("never-created"));

        for &model in ModelId::ALL {
            assert!(!store.is_cached(model));
        }
        Ok(())
    }

    #[test]
    fn located_model_mirrors_its_spec() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let store = ModelStore::new(directory.path());

        for &model in ModelId::ALL {
            let spec = model.spec();
            let local = store.locate(spec);

            let expected: Vec<(PathBuf, StemSelection)> = spec
                .graphs()
                .map(|graph| {
                    (
                        directory.path().join(graph.cache_file_name()),
                        graph.emits(),
                    )
                })
                .collect();
            let actual: Vec<(PathBuf, StemSelection)> = local
                .graphs()
                .map(|graph| (graph.path().to_path_buf(), graph.emits()))
                .collect();
            assert_eq!(actual, expected, "{}", model.name());
        }
        Ok(())
    }

    #[test]
    fn user_cache_store_lives_under_the_application_directory() -> Result<(), ModelStoreError> {
        let store = ModelStore::in_user_cache()?;

        assert!(store.directory().is_absolute());
        assert!(store.directory().ends_with(MODELS_DIRECTORY));
        assert!(
            store
                .directory()
                .components()
                .any(|component| component.as_os_str() == APPLICATION)
        );
        Ok(())
    }

    #[test]
    #[ignore = "downloads 110 MB from huggingface.co"]
    fn real_model_downloads_and_verifies() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let store = ModelStore::new(directory.path());
        let model = ModelId::Htdemucs6s;

        let mut last = None;
        let ensured = store.ensure(
            model,
            &mut |progress| last = Some(progress),
            &CancelToken::new(),
        )?;

        let Ensured::Ready(local) = ensured else {
            return Err("download was reported as cancelled".into());
        };
        assert!(store.is_cached(model));
        assert_eq!(local.graphs().count(), 1);
        assert_eq!(
            last.map(|progress: DownloadProgress| progress.downloaded()),
            Some(model.spec().download_size())
        );
        Ok(())
    }
}
