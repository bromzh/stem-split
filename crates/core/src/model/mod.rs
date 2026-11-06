//! The separation models: what files they consist of and how those files
//! reach the local disk.

mod registry;
mod store;

pub use self::{
    registry::{EnhancerSpec, FileSpec, GraphSpec, ModelSpec, Repository, StemSelection},
    store::{
        CacheError, DownloadProgress, Ensured, LocalEnhancer, LocalFiles, LocalGraph, LocalModel,
        ModelStore, ModelStoreError, Usage,
    },
};
