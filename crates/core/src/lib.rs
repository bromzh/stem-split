//! Audio stem separation pipeline behind the `stem-split` application.
//!
//! The crate has no GUI dependencies: it takes validated requests and reports
//! typed progress, results and errors, leaving presentation to the caller.

pub mod audio;
pub mod domain;
pub mod model;

mod cancel;
mod partial;

pub use cancel::CancelToken;
