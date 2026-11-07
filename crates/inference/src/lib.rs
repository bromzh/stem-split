//! Shared ONNX execution and model-file descriptors; no model-family algorithms.
mod contracts;
mod files;
mod session;
pub use contracts::{ActiveDevice, CpuThreads, DevicePreference, InferenceError, StftError};
pub use files::{FileSpec, Repository};
pub use session::{DeviceSession, TensorInput, TensorOutputs};
