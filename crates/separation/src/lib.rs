//! Independent separation algorithms and model catalog.
mod algorithm;
mod identity;
pub mod model;
pub use algorithm::*;
pub use identity::ModelId;
mod cancel {
    pub(crate) use stem_split_audio::CancelToken;
}
mod domain {
    pub(crate) use stem_split_audio::domain::*;
}
mod audio {
    pub(crate) use stem_split_audio::*;
}
