//! Public model catalog assembled from the independent model families.
pub use stem_split_enhancement::model::EnhancerSpec;
pub use stem_split_inference::{FileSpec, Repository};
pub use stem_split_separation::model::{GraphSpec, ModelSpec, StemSelection};
#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::domain::{EnhancerId, ModelId, Sha256Digest, Stem};

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
