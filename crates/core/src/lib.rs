//! Audio stem separation pipeline behind the `stem-split` application.
//!
//! The crate has no GUI dependencies: it takes validated requests and reports
//! typed progress, results and errors, leaving presentation to the caller.

pub use stem_split_audio as audio;
pub mod batch;
pub mod domain;
pub use stem_split_enhancement as enhance;
pub mod job;
pub mod model;
pub use stem_split_separation as separate;

mod cancel {
    pub(crate) use stem_split_audio::CancelToken;
}
mod partial {
    pub(crate) use stem_split_audio::partial::*;
}

pub use stem_split_audio::{CancelToken, closed_enum};

pub mod config;
pub mod downloads;
mod notification;
pub mod preview;
pub mod request;
pub mod timing;
pub mod tools;
pub mod tracker;
pub mod worker;
pub use notification::Wake;
pub use stem_split_media_fetching as media_fetching;
