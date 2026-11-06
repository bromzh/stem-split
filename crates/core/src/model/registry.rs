//! Static descriptions of the model files.
//!
//! The separation graphs come from the ONNX export at
//! <https://huggingface.co/HRSadeghi/demucs-onnx>, the cleanup graph from the
//! collection of UVR models at <https://huggingface.co/Politrees/UVR_resources>.
//! Sizes and digests were taken from those repositories and checked against
//! downloaded copies.

use std::iter;

use hex_literal::hex;

use crate::{
    closed_enum,
    domain::{ByteCount, EnhancerId, ModelId, Sha256Digest, Stem},
    enhance::{MdxParams, NetworkOutput},
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

/// Every four-stem graph has the same architecture and therefore size.
const FOUR_STEM_GRAPH: ByteCount = ByteCount::new(168_683_222);

/// How many hexadecimal digits of the digest go into a cached file name.
const CACHE_NAME_DIGEST_DIGITS: usize = 16;

/// Which of the stems a graph computes end up in the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StemSelection {
    /// Every stem of the model's layout.
    All,
    /// Only this stem; the graph is a specialist and its other outputs are
    /// discarded.
    Only(Stem),
}

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
    pub(crate) const fn new(
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

/// One ONNX graph of a separation model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphSpec {
    file: FileSpec,
    emits: StemSelection,
}

impl GraphSpec {
    pub(crate) const fn new(file: FileSpec, emits: StemSelection) -> Self {
        Self { file, emits }
    }

    /// A graph of the `HTDemucs` export, which keeps every file at the top
    /// level of its repository.
    const fn demucs(
        name: &'static str,
        size: ByteCount,
        sha256: Sha256Digest,
        emits: StemSelection,
    ) -> Self {
        Self::new(
            FileSpec::new(Repository::DemucsOnnx, "", name, size, sha256),
            emits,
        )
    }

    #[must_use]
    pub const fn file(&self) -> &FileSpec {
        &self.file
    }

    #[must_use]
    pub const fn size(&self) -> ByteCount {
        self.file.size()
    }

    #[must_use]
    pub const fn emits(&self) -> StemSelection {
        self.emits
    }

    #[must_use]
    pub fn cache_file_name(&self) -> String {
        self.file.cache_file_name()
    }
}

/// The graphs a model consists of. Never empty: the first graph is a field
/// of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    first: GraphSpec,
    rest: &'static [GraphSpec],
}

impl ModelSpec {
    #[must_use]
    pub const fn first(&self) -> &GraphSpec {
        &self.first
    }

    /// The graphs after the first.
    #[must_use]
    pub const fn rest(&self) -> &'static [GraphSpec] {
        self.rest
    }

    /// Graphs in the order they are run.
    pub fn graphs(&self) -> impl Iterator<Item = &GraphSpec> {
        iter::once(&self.first).chain(self.rest)
    }

    /// Total size of the model's files.
    #[must_use]
    pub fn download_size(&self) -> ByteCount {
        ByteCount::new(
            self.graphs()
                .fold(0, |total, graph| total.saturating_add(graph.size().get())),
        )
    }
}

static HTDEMUCS: ModelSpec = ModelSpec {
    first: GraphSpec::demucs(
        "htdemucs",
        FOUR_STEM_GRAPH,
        Sha256Digest::new(hex!(
            "abaac48bc8fd55fa672268c8f13228b9b00f52f0c4cd10945048982eca4643bc"
        )),
        StemSelection::All,
    ),
    rest: &[],
};

static HTDEMUCS_6S: ModelSpec = ModelSpec {
    first: GraphSpec::demucs(
        "htdemucs_6s",
        ByteCount::new(110_396_023),
        Sha256Digest::new(hex!(
            "8bf891f10eb208f6fd945f427c8e348ddddef38069d47b8afb7d4aabc24bc15f"
        )),
        StemSelection::All,
    ),
    rest: &[],
};

/// Four specialists, one per stem, in the order of [`StemLayout::FOUR`].
///
/// [`StemLayout::FOUR`]: crate::domain::StemLayout::FOUR
static HTDEMUCS_FT: ModelSpec = ModelSpec {
    first: GraphSpec::demucs(
        "htdemucs_ft_0",
        FOUR_STEM_GRAPH,
        Sha256Digest::new(hex!(
            "4738167ecd97779b0b23e21c754f54d65d7b1e1d077a856ba88295b54a35ba4e"
        )),
        StemSelection::Only(Stem::Drums),
    ),
    rest: &[
        GraphSpec::demucs(
            "htdemucs_ft_1",
            FOUR_STEM_GRAPH,
            Sha256Digest::new(hex!(
                "5f62c38f565e9a73fe60b1917220119fa7b7fc2c259d059a836a9751f0c01072"
            )),
            StemSelection::Only(Stem::Bass),
        ),
        GraphSpec::demucs(
            "htdemucs_ft_2",
            FOUR_STEM_GRAPH,
            Sha256Digest::new(hex!(
                "f3a57a391d1a26cb1009761d664d44420abe22b7842b7f3147d45a1cdde85a7a"
            )),
            StemSelection::Only(Stem::Other),
        ),
        GraphSpec::demucs(
            "htdemucs_ft_3",
            FOUR_STEM_GRAPH,
            Sha256Digest::new(hex!(
                "31baa94de72168b84feba38dbae38c99ac6c3877c4184e5892c837a4ea76f4e5"
            )),
            StemSelection::Only(Stem::Vocals),
        ),
    ],
};

/// A cleanup network: its file and how the host prepares the audio for it
/// and turns its output back into audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnhancerSpec {
    file: FileSpec,
    params: MdxParams,
    output: NetworkOutput,
}

impl EnhancerSpec {
    #[must_use]
    pub const fn file(&self) -> &FileSpec {
        &self.file
    }

    #[must_use]
    pub const fn params(&self) -> MdxParams {
        self.params
    }

    /// What the network outputs, which decides how it cleans each stem and
    /// whether it can clean a mix.
    #[must_use]
    pub const fn output(&self) -> NetworkOutput {
        self.output
    }
}

/// `Reverb_HQ_By_FoxJoy`. The parameters are those of its entry in the
/// model table UVR publishes (`model_data_new.json`, keyed by the MD5 of the
/// last 10 240 000 bytes of the file, here
/// `cd5b2989ad863f116c855db1dfe24e39`): an FFT of 6144 with a hop of 1024,
/// 3072 bins by 512 frames, compensation 1.035, primary stem "Reverb".
static REVERB_HQ: EnhancerSpec = EnhancerSpec {
    file: FileSpec::new(
        Repository::UvrResources,
        "models/MDXNet",
        "Reverb_HQ_By_FoxJoy",
        ByteCount::new(66_780_123),
        Sha256Digest::new(hex!(
            "233bb5c6aaa365e568659a0a81211746fa881f8f47f82d9e864fce1f7692db80"
        )),
    ),
    params: MdxParams::new(6144, 1024, 3072, 512, 1.035),
    output: NetworkOutput::Unwanted,
};

/// `UVR-MDX-NET-Voc_FT`. Its entry in the same table (hash
/// `77d07b2667ddf05b9e3175941b4454a0`): an FFT of 7680 with a hop of 1024,
/// 3072 bins by 256 frames, compensation 1.021, primary stem "Vocals".
/// Chosen over `Kim_Vocal_2`, which has the same shape, because it takes
/// less out of stems that have no vocals; see stage 9a of the plan.
static VOCAL_BLEED: EnhancerSpec = EnhancerSpec {
    file: FileSpec::new(
        Repository::UvrResources,
        "models/MDXNet",
        "UVR-MDX-NET-Voc_FT",
        ByteCount::new(66_762_490),
        Sha256Digest::new(hex!(
            "534b2070fcc7df514b13ef660dc8cbb328679c2374d04354a5c42bb14ecce111"
        )),
    ),
    params: MdxParams::new(7680, 1024, 3072, 256, 1.021),
    output: NetworkOutput::Vocals,
};

impl EnhancerId {
    /// The file of the network and how to run it.
    #[must_use]
    pub const fn spec(self) -> &'static EnhancerSpec {
        match self {
            Self::ReverbHq => &REVERB_HQ,
            Self::VocalBleed => &VOCAL_BLEED,
        }
    }
}

impl ModelId {
    /// The files the model consists of.
    #[must_use]
    pub const fn spec(self) -> &'static ModelSpec {
        match self {
            Self::Htdemucs => &HTDEMUCS,
            Self::HtdemucsFt => &HTDEMUCS_FT,
            Self::Htdemucs6s => &HTDEMUCS_6S,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn download_sizes_match_the_repository() {
        assert_eq!(ModelId::Htdemucs.spec().download_size().get(), 168_683_222);
        assert_eq!(
            ModelId::Htdemucs6s.spec().download_size().get(),
            110_396_023
        );
        assert_eq!(
            ModelId::HtdemucsFt.spec().download_size().get(),
            4 * 168_683_222
        );
    }

    #[test]
    fn file_names_and_digests_are_distinct_across_all_networks() {
        let graphs: Vec<&FileSpec> = ModelId::ALL
            .iter()
            .flat_map(|model| model.spec().graphs().map(GraphSpec::file))
            .chain(EnhancerId::ALL.iter().map(|id| id.spec().file()))
            .collect();
        let names: HashSet<String> = graphs.iter().map(|graph| graph.cache_file_name()).collect();
        let remote: HashSet<String> = graphs.iter().map(|graph| graph.remote_path()).collect();
        let digests: HashSet<Sha256Digest> = graphs.iter().map(|graph| graph.sha256()).collect();

        assert_eq!(graphs.len(), 8);
        assert_eq!(names.len(), graphs.len());
        assert_eq!(remote.len(), graphs.len());
        assert_eq!(digests.len(), graphs.len());
    }

    /// Every stem of a model's layout must come from exactly one graph.
    #[test]
    fn graphs_cover_the_stem_layout_exactly_once() {
        for &model in ModelId::ALL {
            let layout: Vec<Stem> = model.layout().iter().collect();
            let mut produced = Vec::new();
            for graph in model.spec().graphs() {
                match graph.emits() {
                    StemSelection::All => produced.extend(layout.iter().copied()),
                    StemSelection::Only(stem) => produced.push(stem),
                }
            }
            assert_eq!(produced, layout, "{}", model.name());
        }
    }

    #[test]
    fn file_names_follow_the_graph_name() {
        let spec = ModelId::Htdemucs.spec();
        let graph = spec.graphs().next();

        assert_eq!(
            graph.map(|graph| graph.file().remote_path()).as_deref(),
            Some("htdemucs.onnx")
        );
        assert_eq!(
            EnhancerId::ReverbHq.spec().file().remote_path(),
            "models/MDXNet/Reverb_HQ_By_FoxJoy.onnx"
        );
        assert_eq!(
            EnhancerId::ReverbHq.spec().file().cache_file_name(),
            "Reverb_HQ_By_FoxJoy-233bb5c6aaa365e5.onnx"
        );
        assert_eq!(
            graph.map(GraphSpec::cache_file_name).as_deref(),
            Some("htdemucs-abaac48bc8fd55fa.onnx")
        );
    }

    #[test]
    fn download_addresses_are_pinned_to_a_revision_over_https() {
        for repository in Repository::ALL {
            let url = repository.base_url();
            assert!(url.starts_with("https://huggingface.co/"), "{url}");
            assert!(url.contains("/resolve/"), "{url}");
            assert!(!url.contains("/main"), "{url}");
            assert!(!url.ends_with('/'), "{url}");
        }
    }

    #[test]
    fn enhancer_sizes_match_the_repository() {
        assert_eq!(EnhancerId::ReverbHq.spec().file().size().get(), 66_780_123);
        assert_eq!(
            EnhancerId::VocalBleed.spec().file().size().get(),
            66_762_490
        );
    }

    #[test]
    fn enhancer_parameters_describe_a_usable_window() {
        for &id in EnhancerId::ALL {
            assert!(id.spec().params().chunking().is_ok(), "{}", id.name());
        }
    }
}
