//! Download descriptors shared by model families.
use stem_split_audio::{
    closed_enum,
    domain::{ByteCount, Sha256Digest},
};
closed_enum! {
    /// A repository model files are downloaded from.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum Repository {
        /// The `HTDemucs` export with the transforms outside the graph.
        DemucsOnnx,
        /// Public UVR models, among them the MDX-Net cleanup networks.
        UvrResources,
    }
}

impl Repository {
    /// The address files are found under. It names a fixed revision, so the
    /// files behind it cannot change away from the digests recorded here.
    #[must_use]
    pub const fn base_url(self) -> &'static str {
        match self {
            Self::DemucsOnnx => {
                "https://huggingface.co/HRSadeghi/demucs-onnx/resolve/9128f6ee0f1b8e333b5b8d12613aefb3bebc249a"
            }
            Self::UvrResources => {
                "https://huggingface.co/Politrees/UVR_resources/resolve/929e057b81aa49bc2e6490bef8671f47b2c120f6"
            }
        }
    }
}

const CACHE_NAME_DIGEST_DIGITS: usize = 16;

/// One ONNX file to download: where it is, how large it is and what its
/// digest must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSpec {
    repository: Repository,
    /// Folder of the file inside the repository; empty for the top level.
    folder: &'static str,
    /// File name without the `.onnx` extension.
    name: &'static str,
    size: ByteCount,
    sha256: Sha256Digest,
}

impl FileSpec {
    #[must_use]
    pub const fn new(
        repository: Repository,
        folder: &'static str,
        name: &'static str,
        size: ByteCount,
        sha256: Sha256Digest,
    ) -> Self {
        Self {
            repository,
            folder,
            name,
            size,
            sha256,
        }
    }

    #[must_use]
    pub const fn repository(&self) -> Repository {
        self.repository
    }

    /// Size of the file in bytes.
    #[must_use]
    pub const fn size(&self) -> ByteCount {
        self.size
    }

    #[must_use]
    pub const fn sha256(&self) -> Sha256Digest {
        self.sha256
    }

    /// Path of the file inside its repository.
    #[must_use]
    pub fn remote_path(&self) -> String {
        if self.folder.is_empty() {
            format!("{}.onnx", self.name)
        } else {
            format!("{}/{}.onnx", self.folder, self.name)
        }
    }

    /// Name of the file in the local cache.
    ///
    /// It carries the start of the digest, so a file under this name is the
    /// content this spec describes even if a later version of the registry
    /// points the same graph name at different content.
    #[must_use]
    pub fn cache_file_name(&self) -> String {
        let digest: String = self
            .sha256
            .to_string()
            .chars()
            .take(CACHE_NAME_DIGEST_DIGITS)
            .collect();
        format!("{}-{digest}.onnx", self.name)
    }
}
