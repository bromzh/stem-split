//! Static descriptions of the model files.
//!
//! The graphs come from the ONNX export at
//! <https://huggingface.co/HRSadeghi/demucs-onnx>. Sizes and digests were
//! taken from that repository and checked against downloaded copies.

use std::iter;

use hex_literal::hex;

use crate::domain::{ByteCount, ModelId, Sha256Digest, Stem};

/// Where graphs are downloaded from. The address names a fixed revision, so
/// the files behind it cannot change away from the digests recorded here.
pub(super) const BASE_URL: &str =
    "https://huggingface.co/HRSadeghi/demucs-onnx/resolve/9128f6ee0f1b8e333b5b8d12613aefb3bebc249a";

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

/// One ONNX graph of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphSpec {
    name: &'static str,
    size: ByteCount,
    sha256: Sha256Digest,
    emits: StemSelection,
}

impl GraphSpec {
    pub(crate) const fn new(
        name: &'static str,
        size: ByteCount,
        sha256: Sha256Digest,
        emits: StemSelection,
    ) -> Self {
        Self {
            name,
            size,
            sha256,
            emits,
        }
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

    #[must_use]
    pub const fn emits(&self) -> StemSelection {
        self.emits
    }

    /// Name of the file in the model repository.
    #[must_use]
    pub fn remote_file_name(&self) -> String {
        format!("{}.onnx", self.name)
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
                .fold(0, |total, graph| total.saturating_add(graph.size.get())),
        )
    }
}

static HTDEMUCS: ModelSpec = ModelSpec {
    first: GraphSpec::new(
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
    first: GraphSpec::new(
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
    first: GraphSpec::new(
        "htdemucs_ft_0",
        FOUR_STEM_GRAPH,
        Sha256Digest::new(hex!(
            "4738167ecd97779b0b23e21c754f54d65d7b1e1d077a856ba88295b54a35ba4e"
        )),
        StemSelection::Only(Stem::Drums),
    ),
    rest: &[
        GraphSpec::new(
            "htdemucs_ft_1",
            FOUR_STEM_GRAPH,
            Sha256Digest::new(hex!(
                "5f62c38f565e9a73fe60b1917220119fa7b7fc2c259d059a836a9751f0c01072"
            )),
            StemSelection::Only(Stem::Bass),
        ),
        GraphSpec::new(
            "htdemucs_ft_2",
            FOUR_STEM_GRAPH,
            Sha256Digest::new(hex!(
                "f3a57a391d1a26cb1009761d664d44420abe22b7842b7f3147d45a1cdde85a7a"
            )),
            StemSelection::Only(Stem::Other),
        ),
        GraphSpec::new(
            "htdemucs_ft_3",
            FOUR_STEM_GRAPH,
            Sha256Digest::new(hex!(
                "31baa94de72168b84feba38dbae38c99ac6c3877c4184e5892c837a4ea76f4e5"
            )),
            StemSelection::Only(Stem::Vocals),
        ),
    ],
};

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
    fn graph_names_and_digests_are_distinct_across_models() {
        let graphs: Vec<&GraphSpec> = ModelId::ALL
            .iter()
            .flat_map(|model| model.spec().graphs())
            .collect();
        let names: HashSet<String> = graphs.iter().map(|graph| graph.cache_file_name()).collect();
        let remote: HashSet<String> = graphs
            .iter()
            .map(|graph| graph.remote_file_name())
            .collect();
        let digests: HashSet<Sha256Digest> = graphs.iter().map(|graph| graph.sha256()).collect();

        assert_eq!(graphs.len(), 6);
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
            graph.map(GraphSpec::remote_file_name).as_deref(),
            Some("htdemucs.onnx")
        );
        assert_eq!(
            graph.map(GraphSpec::cache_file_name).as_deref(),
            Some("htdemucs-abaac48bc8fd55fa.onnx")
        );
    }

    #[test]
    fn download_address_is_pinned_to_a_revision_over_https() {
        assert!(BASE_URL.starts_with("https://huggingface.co/"));
        assert!(!BASE_URL.contains("/main"));
        assert!(!BASE_URL.ends_with('/'));
    }
}
