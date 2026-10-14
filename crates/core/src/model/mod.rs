//! The separation models: what files they consist of and how those files
//! reach the local disk.

mod registry;
mod store;

pub use self::{
    registry::{GraphSpec, ModelSpec, StemSelection},
    store::{
        CacheError, DownloadProgress, Ensured, LocalGraph, LocalModel, ModelStore, ModelStoreError,
        Usage,
    },
};
