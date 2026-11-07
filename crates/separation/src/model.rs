//! Pinned `HTDemucs` graph descriptions.
use crate::ModelId;
use hex_literal::hex;
use std::iter;
use stem_split_audio::domain::{ByteCount, Sha256Digest, Stem};
use stem_split_inference::{FileSpec, Repository};
const FOUR_STEM_GRAPH: ByteCount = ByteCount::new(168_683_222);
/// Which of the stems a graph computes end up in the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StemSelection {
    /// Every stem of the model's layout.
    All,
    /// Only this stem; the graph is a specialist and its other outputs are
    /// discarded.
    Only(Stem),
}

/// One ONNX graph of a separation model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphSpec {
    file: FileSpec,
    emits: StemSelection,
}

impl GraphSpec {
    #[must_use]
    pub const fn new(file: FileSpec, emits: StemSelection) -> Self {
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
