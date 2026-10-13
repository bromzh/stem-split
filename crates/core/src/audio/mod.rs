//! Getting audio into memory at the model's sample rate, and stems onto disk.
//!
//! The stages hand each other distinct types, so they cannot be applied out
//! of order: [`decode`] yields [`DecodedAudio`] at whatever rate the file
//! has, and only [`resample`] turns that into [`ModelAudio`].

mod decode;
mod resample;
mod wav;

use std::time::Duration;

pub use self::{
    decode::{DecodeError, decode},
    resample::{ResampleError, resample},
    wav::{WavWriter, WriteError},
};
use crate::domain::{SampleRate, StereoFrames};

/// Longest input the application accepts.
///
/// The input is held in memory as a whole, and three hours of stereo at the
/// model rate is also about what fits in a 32-bit float WAV file.
pub const MAX_DURATION: Duration = Duration::from_hours(3);

/// Stereo audio at the sample rate of the file it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudio {
    rate: SampleRate,
    frames: StereoFrames,
}

impl DecodedAudio {
    #[must_use]
    pub const fn new(rate: SampleRate, frames: StereoFrames) -> Self {
        Self { rate, frames }
    }

    #[must_use]
    pub const fn rate(&self) -> SampleRate {
        self.rate
    }

    #[must_use]
    pub const fn frames(&self) -> &StereoFrames {
        &self.frames
    }

    #[must_use]
    pub fn into_parts(self) -> (SampleRate, StereoFrames) {
        (self.rate, self.frames)
    }
}

/// Stereo audio at [`SampleRate::MODEL`], ready for separation.
///
/// The rate is not stored: the only way to get a value is [`resample`], so
/// holding one is the proof.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelAudio(StereoFrames);

impl ModelAudio {
    /// Wraps frames the caller knows to be at [`SampleRate::MODEL`].
    pub(crate) const fn from_frames_at_model_rate(frames: StereoFrames) -> Self {
        Self(frames)
    }

    #[must_use]
    pub const fn frames(&self) -> &StereoFrames {
        &self.0
    }

    #[must_use]
    pub fn into_frames(self) -> StereoFrames {
        self.0
    }
}
