use super::units::FrameCount;

/// Stereo PCM audio as one `[left, right]` pair per frame.
///
/// The element type makes "two channels of equal length" the only shape the
/// data can have, so nothing downstream checks channel counts or lengths.
#[derive(Debug, Clone, PartialEq)]
pub struct StereoFrames(Vec<[f32; 2]>);

/// The two channels passed to [`StereoFrames::from_planar`] differ in length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("channels differ in length: left has {left} samples, right has {right}")]
pub struct ChannelLengthMismatch {
    pub left: usize,
    pub right: usize,
}

impl StereoFrames {
    #[must_use]
    pub const fn new(frames: Vec<[f32; 2]>) -> Self {
        Self(frames)
    }

    /// Interleaves two channels stored separately.
    ///
    /// # Errors
    ///
    /// Returns [`ChannelLengthMismatch`] if the channels differ in length.
    pub fn from_planar(left: &[f32], right: &[f32]) -> Result<Self, ChannelLengthMismatch> {
        if left.len() != right.len() {
            return Err(ChannelLengthMismatch {
                left: left.len(),
                right: right.len(),
            });
        }
        Ok(Self(
            left.iter().zip(right).map(|(&l, &r)| [l, r]).collect(),
        ))
    }

    /// Plays a mono signal on both channels.
    #[must_use]
    pub fn from_mono(samples: &[f32]) -> Self {
        Self(samples.iter().map(|&sample| [sample, sample]).collect())
    }

    #[must_use]
    pub const fn len(&self) -> FrameCount {
        FrameCount::new(self.0.len())
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub const fn as_slice(&self) -> &[[f32; 2]] {
        self.0.as_slice()
    }

    #[must_use]
    pub fn into_inner(self) -> Vec<[f32; 2]> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planar_channels_are_interleaved() -> Result<(), ChannelLengthMismatch> {
        let frames = StereoFrames::from_planar(&[1.0, 2.0, 3.0], &[-1.0, -2.0, -3.0])?;
        assert_eq!(frames.as_slice(), [[1.0, -1.0], [2.0, -2.0], [3.0, -3.0]]);
        assert_eq!(frames.len(), FrameCount::new(3));
        Ok(())
    }

    #[test]
    fn planar_channels_of_different_length_are_rejected() {
        assert_eq!(
            StereoFrames::from_planar(&[1.0, 2.0], &[1.0]),
            Err(ChannelLengthMismatch { left: 2, right: 1 })
        );
    }

    #[test]
    fn mono_is_duplicated_into_both_channels() {
        let frames = StereoFrames::from_mono(&[0.5, -0.25]);
        assert_eq!(frames.as_slice(), [[0.5, 0.5], [-0.25, -0.25]]);
    }

    #[test]
    fn empty_input_gives_empty_audio() -> Result<(), ChannelLengthMismatch> {
        assert!(StereoFrames::from_planar(&[], &[])?.is_empty());
        assert!(StereoFrames::from_mono(&[]).is_empty());
        assert_eq!(StereoFrames::new(Vec::new()).len(), FrameCount::ZERO);
        Ok(())
    }

    #[test]
    fn into_inner_returns_the_frames() {
        let frames = StereoFrames::new(vec![[0.1, 0.2]]);
        assert_eq!(frames.into_inner(), [[0.1, 0.2]]);
    }
}
