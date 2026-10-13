//! Audio stem separation pipeline behind the `stem-split` application.
//!
//! The crate has no GUI dependencies: it takes validated requests and reports
//! typed progress, results and errors, leaving presentation to the caller.

pub mod audio;
pub mod domain;
pub mod job;
pub mod model;
pub mod separate;

mod cancel;
mod partial;

pub use cancel::CancelToken;
