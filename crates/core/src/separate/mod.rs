//! Splitting audio at the model rate into stems.
//!
//! The track is cut into overlapping segments ([`plan`]), each segment goes
//! through a [`Separator`], and the results are blended back into continuous
//! stems ([`overlap`]). Everything except the separator itself is plain
//! arithmetic and is tested without a model.

mod backend;
mod overlap;
pub(crate) mod pipeline;
pub(crate) mod plan;
mod stft;

use std::num::NonZeroUsize;

pub use self::backend::{ActiveDevice, CpuThreads, DevicePreference, InferenceError, OrtSeparator};

/// Length of the window every supported model works on: 7.8 s at 44.1 kHz.
pub const SEGMENT_FRAMES: usize = 343_980;

const CHANNELS: usize = 2;

/// One model window of normalized stereo audio, channels stored separately.
///
/// Both channels hold exactly [`SEGMENT_FRAMES`] samples: a segment starts
/// out silent and is only ever written into, never resized.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    left: Vec<f32>,
    right: Vec<f32>,
}

impl Segment {
    pub(crate) fn silent() -> Self {
        Self {
            left: vec![0.0; SEGMENT_FRAMES],
            right: vec![0.0; SEGMENT_FRAMES],
        }
    }

    pub(crate) fn channels_mut(&mut self) -> (&mut [f32], &mut [f32]) {
        (&mut self.left, &mut self.right)
    }

    #[must_use]
    pub fn left(&self) -> &[f32] {
        &self.left
    }

    #[must_use]
    pub fn right(&self) -> &[f32] {
        &self.right
    }
}

/// What a model computes for one [`Segment`]: every stem of its layout, each
/// as two channels of [`SEGMENT_FRAMES`] samples.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkStems {
    stems: NonZeroUsize,
    samples: Vec<f32>,
}

impl ChunkStems {
    /// Wraps samples laid out as `[stems][2][SEGMENT_FRAMES]`. Returns `None`
    /// if the length does not match that layout.
    #[must_use]
    pub fn new(stems: NonZeroUsize, samples: Vec<f32>) -> Option<Self> {
        let expected = stems
            .get()
            .checked_mul(CHANNELS)?
            .checked_mul(SEGMENT_FRAMES)?;
        (samples.len() == expected).then_some(Self { stems, samples })
    }

    #[must_use]
    pub const fn stem_count(&self) -> NonZeroUsize {
        self.stems
    }

    /// Left and right channel of the stem at `index` in the model's layout.
    #[must_use]
    pub fn stem(&self, index: usize) -> Option<(&[f32], &[f32])> {
        self.samples
            .as_chunks::<{ CHANNELS * SEGMENT_FRAMES }>()
            .0
            .get(index)?
            .split_at_checked(SEGMENT_FRAMES)
    }

    /// All samples, laid out as `[stems][2][SEGMENT_FRAMES]`.
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

/// Turns one segment of a mix into its stems.
///
/// An implementation stands for one loaded graph. The rest of the pipeline
/// knows nothing about how the stems are computed.
pub trait Separator {
    /// # Errors
    ///
    /// Returns an error if the stems cannot be computed.
    fn separate(&mut self, segment: &Segment) -> Result<ChunkStems, InferenceError>;

    /// The device the next segment will be computed on.
    fn device(&self) -> ActiveDevice;
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A separator whose stem `k` is the input scaled by `k + 1`. With one
    /// stem it is the identity, which makes the blending around it testable.
    #[derive(Debug)]
    pub(crate) struct ScalingSeparator {
        pub(crate) stems: NonZeroUsize,
        pub(crate) calls: usize,
    }

    impl ScalingSeparator {
        pub(crate) const fn new(stems: NonZeroUsize) -> Self {
            Self { stems, calls: 0 }
        }
    }

    #[expect(clippy::cast_precision_loss, reason = "stem indices are single digits")]
    impl Separator for ScalingSeparator {
        fn separate(&mut self, segment: &Segment) -> Result<ChunkStems, InferenceError> {
            self.calls += 1;
            let samples: Vec<f32> = (0..self.stems.get())
                .flat_map(|stem| {
                    let gain = (stem + 1) as f32;
                    segment
                        .left()
                        .iter()
                        .chain(segment.right())
                        .map(move |&sample| sample * gain)
                })
                .collect();
            ChunkStems::new(self.stems, samples).ok_or(InferenceError::OutputShape {
                tensor: "test",
                shape: Vec::new(),
            })
        }

        fn device(&self) -> ActiveDevice {
            ActiveDevice::Cpu
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_segment_has_the_model_window_length() {
        let segment = Segment::silent();
        assert_eq!(segment.left().len(), SEGMENT_FRAMES);
        assert_eq!(segment.right().len(), SEGMENT_FRAMES);
        assert!(segment.left().iter().all(|&sample| sample == 0.0));
    }

    #[test]
    fn chunk_stems_reject_a_wrong_sample_count() {
        let two = NonZeroUsize::MIN.saturating_add(1);
        let per_stem = CHANNELS * SEGMENT_FRAMES;

        assert!(ChunkStems::new(two, vec![0.0; 2 * per_stem]).is_some());
        assert!(ChunkStems::new(two, vec![0.0; per_stem]).is_none());
        assert!(ChunkStems::new(two, vec![0.0; 2 * per_stem + 1]).is_none());
        assert!(ChunkStems::new(two, Vec::new()).is_none());
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the values are copied or scaled by powers of two, so they are exact"
    )]
    fn stems_are_addressed_by_layout_index() -> Result<(), &'static str> {
        let two = NonZeroUsize::MIN.saturating_add(1);
        let per_stem = CHANNELS * SEGMENT_FRAMES;
        // Stem 0: left 1, right 2. Stem 1: left 3, right 4.
        let samples: Vec<f32> = [1.0, 2.0, 3.0, 4.0]
            .into_iter()
            .flat_map(|value| std::iter::repeat_n(value, SEGMENT_FRAMES))
            .collect();
        assert_eq!(samples.len(), 2 * per_stem);
        let stems = ChunkStems::new(two, samples).ok_or("layout")?;

        let (left, right) = stems.stem(1).ok_or("stem 1")?;
        assert_eq!(left.len(), SEGMENT_FRAMES);
        assert_eq!(right.len(), SEGMENT_FRAMES);
        assert!(left.iter().all(|&sample| sample == 3.0));
        assert!(right.iter().all(|&sample| sample == 4.0));
        assert!(stems.stem(2).is_none());
        Ok(())
    }
}
