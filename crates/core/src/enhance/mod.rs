//! Cleaning separated audio with an MDX-Net network.
//!
//! An MDX-Net graph takes the spectrogram of a fixed window of stereo audio
//! and returns a spectrogram of the same shape. The host computes both
//! transforms ([`stft`]) and cuts the track into windows the way UVR does:
//! each window overlaps its neighbours by half an FFT on either side, and
//! only its middle is kept, so the windows join without blending.
//!
//! What a network outputs ([`NetworkOutput`]) decides, for each stem or mix
//! it cleans, whether its output is what the cleanup keeps or what it
//! removes ([`Prediction`]). Everything except the network itself is plain
//! arithmetic and is tested without a model.

mod backend;
mod stft;

use std::num::NonZeroUsize;

pub use self::backend::OrtEnhancer;
use crate::{
    cancel::CancelToken,
    domain::{Stem, StemSet},
    separate::{ActiveDevice, InferenceError, StftError},
};

/// How the network's output becomes the cleaned audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prediction {
    /// The output is the cleaned audio itself.
    Kept,
    /// The output is what is to be removed; the cleaned audio is the input
    /// minus it.
    Removed,
}

/// What a cleanup network outputs, which decides how it cleans a stem or a
/// mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkOutput {
    /// Something unwanted in any audio, such as reverb: always removed.
    Unwanted,
    /// The vocals. They are what remains of the vocals stem, which loses
    /// the instruments leaked into it, and what is removed from any other
    /// stem, which loses the vocals leaked into it.
    Vocals,
}

impl NetworkOutput {
    /// How the output cleans `stem`.
    #[must_use]
    pub const fn for_stem(self, stem: Stem) -> Prediction {
        match (self, stem) {
            (Self::Unwanted, _)
            | (Self::Vocals, Stem::Drums | Stem::Bass | Stem::Other | Stem::Guitar | Stem::Piano) => {
                Prediction::Removed
            }
            (Self::Vocals, Stem::Vocals) => Prediction::Kept,
        }
    }

    /// How the output cleans a mix of `stems`; `None` when it cannot: a
    /// network that tells vocals from instruments cannot clean a mix that
    /// is meant to have both.
    #[must_use]
    pub fn for_mix(self, stems: StemSet) -> Option<Prediction> {
        let mut predictions = stems.iter_all().map(|stem| self.for_stem(stem));
        let first = predictions.next()?;
        predictions
            .all(|prediction| prediction == first)
            .then_some(first)
    }
}

/// How an MDX-Net network is fed: the transform, the size of the window it
/// works on, and how its output becomes audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MdxParams {
    n_fft: usize,
    hop: usize,
    /// Frequency bins the network sees, counted from zero; the ones above
    /// are dropped on the way in and restored as silence on the way out.
    bins: usize,
    /// Spectrogram frames of one window.
    frames: usize,
    /// Gain applied to the network's output, as UVR does.
    compensate: f32,
}

/// How a track is cut into the windows a network works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Chunking {
    /// Frames of one window.
    window: usize,
    /// Frames dropped at each edge of a window's output: half an FFT,
    /// which the transform cannot reconstruct cleanly.
    trim: usize,
    /// Frames of the track each window yields: its middle.
    yields: NonZeroUsize,
}

impl Chunking {
    /// Windows needed for a track of `frames` frames.
    pub(crate) const fn windows(&self, frames: usize) -> usize {
        frames.div_ceil(self.yields.get())
    }
}

impl MdxParams {
    pub(crate) const fn new(
        n_fft: usize,
        hop: usize,
        bins: usize,
        frames: usize,
        compensate: f32,
    ) -> Self {
        Self {
            n_fft,
            hop,
            bins,
            frames,
            compensate,
        }
    }

    /// The window the network works on, if the parameters describe one:
    /// a hop and an FFT that divide the window, bins the FFT has, and a
    /// window longer than the margins it loses.
    ///
    /// # Errors
    ///
    /// Returns [`StftError::TooShort`] when the window is too short for the
    /// transform; for the networks of the registry this does not happen,
    /// which a test checks.
    pub(crate) fn chunking(&self) -> Result<Chunking, InferenceError> {
        let window = self.hop.saturating_mul(self.frames.saturating_sub(1));
        let trim = self.n_fft / 2;
        let usable = self.hop > 0
            && self.n_fft.is_multiple_of(2)
            && self.bins > 0
            && self.bins <= trim.saturating_add(1)
            && window > trim;
        let yields = window
            .checked_sub(trim.saturating_mul(2))
            .and_then(NonZeroUsize::new)
            .filter(|_| usable)
            .ok_or(StftError::TooShort(window))?;
        Ok(Chunking {
            window,
            trim,
            yields,
        })
    }

    pub(crate) const fn n_fft(&self) -> usize {
        self.n_fft
    }

    pub(crate) const fn hop(&self) -> usize {
        self.hop
    }

    pub(crate) const fn bins(&self) -> usize {
        self.bins
    }

    pub(crate) const fn frames(&self) -> usize {
        self.frames
    }

    /// The cleaned frame from an input frame and the network's output for it.
    fn combine(
        &self,
        prediction: Prediction,
        [left, right]: [f32; 2],
        [out_left, out_right]: [f32; 2],
    ) -> [f32; 2] {
        let gain = self.compensate;
        match prediction {
            Prediction::Kept => [out_left * gain, out_right * gain],
            Prediction::Removed => [left - out_left * gain, right - out_right * gain],
        }
    }
}

/// Turns one window of audio into the network's output for it.
///
/// An implementation stands for one loaded graph. The rest of the cleanup
/// knows nothing about how the output is computed.
pub trait Enhancer {
    /// The output for `window`, which holds exactly as many frames as the
    /// network's window.
    ///
    /// # Errors
    ///
    /// Returns an error if the output cannot be computed.
    fn predict(&mut self, window: &[[f32; 2]]) -> Result<Vec<[f32; 2]>, InferenceError>;

    /// The device the next window will be computed on.
    fn device(&self) -> ActiveDevice;
}

/// How cleaning a track ended, short of an error.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Cleaned {
    Completed(Vec<[f32; 2]>),
    Cancelled,
}

/// Cleans `frames` with `enhancer`, whose output is used as `prediction`
/// says.
///
/// `cancel` is checked before each window, so cleaning stops within one run
/// of the network. `on_window` is called after each window with the device
/// that computed it.
///
/// # Errors
///
/// Returns an error if the network fails or returns a window of the wrong
/// length.
pub(crate) fn clean(
    frames: &[[f32; 2]],
    params: &MdxParams,
    prediction: Prediction,
    enhancer: &mut dyn Enhancer,
    on_window: &mut dyn FnMut(ActiveDevice),
    cancel: &CancelToken,
) -> Result<Cleaned, InferenceError> {
    let chunking = params.chunking()?;
    let yields = chunking.yields.get();
    let mut cleaned = Vec::with_capacity(frames.len());
    let mut window = vec![[0.0_f32; 2]; chunking.window];

    for start in (0..frames.len()).step_by(yields) {
        if cancel.is_cancelled() {
            return Ok(Cleaned::Cancelled);
        }
        // The window begins half an FFT before the frames it yields; outside
        // the track it is silent.
        for (offset, slot) in window.iter_mut().enumerate() {
            let position = (start + offset).checked_sub(chunking.trim);
            *slot = position
                .and_then(|position| frames.get(position))
                .copied()
                .unwrap_or([0.0; 2]);
        }
        let device = enhancer.device();
        let output = enhancer.predict(&window)?;
        if output.len() != window.len() {
            return Err(StftError::PlaneSize {
                expected: window.len(),
                actual: output.len(),
            }
            .into());
        }
        let middle = output
            .get(chunking.trim..chunking.trim.saturating_add(yields))
            .ok_or(StftError::PlaneSize {
                expected: chunking.window,
                actual: output.len(),
            })?;
        let input = frames.get(start..).unwrap_or_default();
        cleaned.extend(
            input
                .iter()
                .zip(middle)
                .map(|(&input, &output)| params.combine(prediction, input, output)),
        );
        on_window(device);
    }
    Ok(Cleaned::Completed(cleaned))
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// An enhancer whose output is its input times `gain`.
    #[derive(Debug)]
    pub(crate) struct ScalingEnhancer {
        pub(crate) gain: f32,
        pub(crate) calls: usize,
    }

    impl ScalingEnhancer {
        pub(crate) const fn new(gain: f32) -> Self {
            Self { gain, calls: 0 }
        }
    }

    impl Enhancer for ScalingEnhancer {
        fn predict(&mut self, window: &[[f32; 2]]) -> Result<Vec<[f32; 2]>, InferenceError> {
            self.calls += 1;
            Ok(window
                .iter()
                .map(|&[left, right]| [left * self.gain, right * self.gain])
                .collect())
        }

        fn device(&self) -> ActiveDevice {
            ActiveDevice::Cpu
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::{testing::ScalingEnhancer, *};

    /// A small network: windows of 1023 × 16 = 16 368 frames, of which the
    /// middle 16 368 − 64 = 16 304 are kept.
    const SMALL: MdxParams = MdxParams::new(64, 16, 32, 1024, 1.0);

    #[expect(clippy::cast_precision_loss, reason = "test ramp of small integers")]
    fn ramp(length: usize) -> Vec<[f32; 2]> {
        (0..length)
            .map(|index| {
                let value = (index % 997) as f32 / 997.0 - 0.5;
                [value, -0.5 * value]
            })
            .collect()
    }

    fn worst(actual: &[[f32; 2]], expected: impl Iterator<Item = [f32; 2]>) -> f32 {
        actual
            .iter()
            .zip(expected)
            .flat_map(|(a, e)| [(a[0] - e[0]).abs(), (a[1] - e[1]).abs()])
            .fold(0.0, f32::max)
    }

    #[test]
    fn unwanted_output_is_always_removed() {
        for &stem in Stem::ALL {
            assert_eq!(NetworkOutput::Unwanted.for_stem(stem), Prediction::Removed);
        }
        let everything = Stem::ALL
            .iter()
            .fold(StemSet::none(), |set, &stem| set.toggled(stem));
        assert_eq!(
            NetworkOutput::Unwanted.for_mix(everything),
            Some(Prediction::Removed)
        );
    }

    #[test]
    fn vocals_are_kept_in_the_vocals_and_removed_elsewhere() {
        let vocals = NetworkOutput::Vocals;
        assert_eq!(vocals.for_stem(Stem::Vocals), Prediction::Kept);
        for stem in [
            Stem::Drums,
            Stem::Bass,
            Stem::Other,
            Stem::Guitar,
            Stem::Piano,
        ] {
            assert_eq!(vocals.for_stem(stem), Prediction::Removed, "{stem:?}");
        }

        let instruments = StemSet::none().toggled(Stem::Drums).toggled(Stem::Piano);
        let only_vocals = StemSet::none().toggled(Stem::Vocals);
        assert_eq!(vocals.for_mix(instruments), Some(Prediction::Removed));
        assert_eq!(vocals.for_mix(only_vocals), Some(Prediction::Kept));
        // A mix meant to have both cannot be told apart.
        assert_eq!(vocals.for_mix(instruments.toggled(Stem::Vocals)), None);
        assert_eq!(vocals.for_mix(StemSet::none()), None);
    }

    #[test]
    fn registry_window_matches_the_network() -> Result<(), InferenceError> {
        let params = MdxParams::new(6144, 1024, 3072, 512, 1.035);
        let chunking = params.chunking()?;

        // 511 hops of 1024: 11.9 s at 44.1 kHz, less half an FFT each side.
        assert_eq!(chunking.window, 523_264);
        assert_eq!(chunking.trim, 3072);
        assert_eq!(chunking.yields.get(), 517_120);
        assert_eq!(chunking.windows(1), 1);
        assert_eq!(chunking.windows(517_120), 1);
        assert_eq!(chunking.windows(517_121), 2);
        Ok(())
    }

    #[test]
    fn unusable_parameters_are_rejected() {
        let short = MdxParams::new(6144, 1024, 3072, 6, 1.0);
        let too_many_bins = MdxParams::new(64, 16, 34, 1024, 1.0);
        let odd_fft = MdxParams::new(63, 16, 32, 1024, 1.0);

        for params in [short, too_many_bins, odd_fft] {
            assert!(params.chunking().is_err(), "{params:?}");
        }
    }

    #[test]
    fn identity_network_returns_the_input() -> Result<(), Box<dyn Error>> {
        for length in [1, 100, 16_304, 16_305, 50_000] {
            let input = ramp(length);
            let mut enhancer = ScalingEnhancer::new(1.0);
            let mut windows = 0;

            let Cleaned::Completed(output) = clean(
                &input,
                &SMALL,
                Prediction::Kept,
                &mut enhancer,
                &mut |_| windows += 1,
                &CancelToken::new(),
            )?
            else {
                return Err("cleaning was cancelled".into());
            };

            assert_eq!(output.len(), length);
            assert!(
                worst(&output, input.iter().copied()) < f32::EPSILON,
                "{length}"
            );
            assert_eq!(windows, length.div_ceil(16_304));
            assert_eq!(enhancer.calls, windows);
        }
        Ok(())
    }

    #[test]
    fn removed_prediction_is_subtracted_with_compensation() -> Result<(), Box<dyn Error>> {
        let compensate = 1.035_f32;
        let params = MdxParams::new(64, 16, 32, 1024, compensate);
        let input = ramp(40_000);
        // A network that predicts the whole input, less the compensation,
        // as what is to be removed: nothing is left.
        let mut enhancer = ScalingEnhancer::new(1.0 / compensate);

        let end = clean(
            &input,
            &params,
            Prediction::Removed,
            &mut enhancer,
            &mut |_| {},
            &CancelToken::new(),
        )?;

        let Cleaned::Completed(output) = end else {
            return Err("cleaning was cancelled".into());
        };
        assert!(worst(&output, std::iter::repeat([0.0; 2])) < 1e-6);
        Ok(())
    }

    #[test]
    fn kept_prediction_is_the_output_with_compensation() -> Result<(), Box<dyn Error>> {
        let params = MdxParams::new(64, 16, 32, 1024, 2.0);
        let input = ramp(20_000);
        let mut enhancer = ScalingEnhancer::new(0.25);

        let end = clean(
            &input,
            &params,
            Prediction::Kept,
            &mut enhancer,
            &mut |_| {},
            &CancelToken::new(),
        )?;

        let Cleaned::Completed(output) = end else {
            return Err("cleaning was cancelled".into());
        };
        let expected = input.iter().map(|&[left, right]| [left * 0.5, right * 0.5]);
        assert!(worst(&output, expected) < 1e-7);
        Ok(())
    }

    #[test]
    fn cancelled_cleaning_stops_before_the_next_window() -> Result<(), InferenceError> {
        let input = ramp(100_000);
        let cancel = CancelToken::new();
        let mut enhancer = ScalingEnhancer::new(1.0);

        let end = clean(
            &input,
            &SMALL,
            Prediction::Kept,
            &mut enhancer,
            &mut |_| cancel.cancel(),
            &cancel,
        )?;

        assert_eq!(end, Cleaned::Cancelled);
        assert_eq!(enhancer.calls, 1);
        Ok(())
    }

    #[test]
    fn window_of_the_wrong_length_is_an_error() {
        struct Short;
        impl Enhancer for Short {
            fn predict(&mut self, window: &[[f32; 2]]) -> Result<Vec<[f32; 2]>, InferenceError> {
                Ok(window.iter().skip(1).copied().collect())
            }
            fn device(&self) -> ActiveDevice {
                ActiveDevice::Cpu
            }
        }

        let result = clean(
            &ramp(1000),
            &SMALL,
            Prediction::Kept,
            &mut Short,
            &mut |_| {},
            &CancelToken::new(),
        );

        assert!(matches!(result, Err(InferenceError::Stft(_))));
    }
}
