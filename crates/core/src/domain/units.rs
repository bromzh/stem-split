use std::num::NonZeroU32;

/// Audio sampling rate in hertz. Never zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SampleRate(NonZeroU32);

impl SampleRate {
    /// The rate every supported model works at: 44.1 kHz.
    // Counting up from `NonZeroU32::MIN` builds the constant without a
    // fallible conversion from a plain integer.
    pub const MODEL: Self = Self(NonZeroU32::MIN.saturating_add(44_099));

    #[must_use]
    pub const fn new(hertz: NonZeroU32) -> Self {
        Self(hertz)
    }

    /// Returns `None` for a rate of zero.
    #[must_use]
    pub fn from_hertz(hertz: u32) -> Option<Self> {
        NonZeroU32::new(hertz).map(Self)
    }

    #[must_use]
    pub const fn hertz(self) -> NonZeroU32 {
        self.0
    }
}

/// A number of audio frames. One frame holds one sample per channel, so the
/// count does not depend on the channel layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameCount(usize);

impl FrameCount {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn new(frames: usize) -> Self {
        Self(frames)
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A size in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteCount(u64);

impl ByteCount {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_rate_is_44100_hertz() {
        assert_eq!(SampleRate::MODEL.hertz().get(), 44_100);
        assert_eq!(SampleRate::from_hertz(44_100), Some(SampleRate::MODEL));
    }

    #[test]
    fn zero_rate_is_rejected() {
        assert_eq!(SampleRate::from_hertz(0), None);
    }

    #[test]
    fn rates_order_by_frequency() {
        let low = SampleRate::from_hertz(22_050);
        let high = SampleRate::from_hertz(48_000);
        assert!(low < high);
        assert!(low < Some(SampleRate::MODEL));
    }

    #[test]
    fn counts_keep_their_value() {
        assert_eq!(FrameCount::new(343_980).get(), 343_980);
        assert_eq!(FrameCount::ZERO.get(), 0);
        assert_eq!(ByteCount::new(168_683_222).get(), 168_683_222);
        assert_eq!(ByteCount::ZERO.get(), 0);
    }
}
