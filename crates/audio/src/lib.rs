//! Audio types, decoding, resampling and atomic output, independent of inference and UI.
pub mod audio;
mod cancel;
pub mod domain;
pub mod partial;
pub use audio::*;
pub use cancel::CancelToken;

/// A destination for a stream of completed stereo frames.
pub trait FrameSink {
    /// # Errors
    /// Returns an output error if the frames cannot be written.
    fn write(&mut self, frames: &[[f32; 2]]) -> Result<(), WriteError>;
}
impl FrameSink for WavWriter {
    fn write(&mut self, frames: &[[f32; 2]]) -> Result<(), WriteError> {
        Self::write(self, frames)
    }
}
#[cfg(any(test, feature = "test-support"))]
mod testing {
    use super::{FrameSink, WriteError};

    /// Collects frames in memory.
    impl FrameSink for Vec<[f32; 2]> {
        fn write(&mut self, frames: &[[f32; 2]]) -> Result<(), WriteError> {
            self.extend_from_slice(frames);
            Ok(())
        }
    }
}

#[cfg(feature = "ffmpeg")]
pub mod ffmpeg;
#[cfg(feature = "ffmpeg")]
pub mod process;
