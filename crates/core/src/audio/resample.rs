use rubato::{
    Fft, FixedSync, ResampleError as ProcessError, Resampler, ResamplerConstructionError,
    WindowFunction,
    audioadapter_buffers::{SizeError, direct::InterleavedSlice},
};

use super::{DecodedAudio, ModelAudio};
use crate::domain::{SampleRate, StereoFrames};

const CHANNELS: usize = 2;

/// Frames per processing block.
///
/// The whole clip is resampled offline, so the delay a large block adds does
/// not matter, while a longer transform gives the anti-aliasing filter a
/// steeper slope and keeps more of the top octave.
const BLOCK_FRAMES: usize = 4096;

/// One transform per block, for the same reason.
const SUB_BLOCKS: usize = 1;

/// Why audio could not be brought to the model's sample rate.
#[derive(Debug, thiserror::Error)]
pub enum ResampleError {
    #[error("a sample rate of {} Hz cannot be converted", rate.hertz())]
    UnsupportedRate {
        rate: SampleRate,
        #[source]
        source: ResamplerConstructionError,
    },
    #[error("a sample rate of {} Hz does not fit this platform's address space", rate.hertz())]
    RateOutOfRange { rate: SampleRate },
    #[error("resampling failed: {0}")]
    Process(#[from] ProcessError),
    #[error("resampling buffer has the wrong size: {0}")]
    Buffer(#[from] SizeError),
}

fn hertz(rate: SampleRate) -> Result<usize, ResampleError> {
    usize::try_from(rate.hertz().get()).map_err(|_| ResampleError::RateOutOfRange { rate })
}

/// Converts audio to [`SampleRate::MODEL`], keeping its duration and timing.
///
/// Audio already at the model rate is passed through untouched. Otherwise the
/// result has `ceil(frames * 44100 / rate)` frames, with the resampler's own
/// delay removed so that the output starts where the input does.
///
/// # Errors
///
/// Returns an error if a resampler cannot be built for the input rate or
/// fails while processing.
pub fn resample(audio: DecodedAudio) -> Result<ModelAudio, ResampleError> {
    let (rate, frames) = audio.into_parts();
    if rate == SampleRate::MODEL {
        return Ok(ModelAudio::from_frames_at_model_rate(frames));
    }

    let mut resampler = Fft::<f32>::new_custom(
        hertz(rate)?,
        hertz(SampleRate::MODEL)?,
        BLOCK_FRAMES,
        SUB_BLOCKS,
        CHANNELS,
        WindowFunction::BlackmanHarris2,
        FixedSync::Both,
    )
    .map_err(|source| ResampleError::UnsupportedRate { rate, source })?;

    let frame_count = frames.len().get();
    let input = InterleavedSlice::new(frames.as_slice().as_flattened(), CHANNELS, frame_count)?;
    let output = resampler.process_all(&input, frame_count, None)?;

    let samples = output.take_data();
    let (pairs, _) = samples.as_chunks::<CHANNELS>();
    Ok(ModelAudio::from_frames_at_model_rate(StereoFrames::new(
        pairs.to_vec(),
    )))
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;

    /// A sine on the left channel and silence on the right.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "test signal: sample indices are small and f32 is the sample type"
    )]
    fn tone(rate: u32, frequency: f64, frames: usize) -> DecodedAudio {
        let step = TAU * frequency / f64::from(rate);
        let samples: Vec<[f32; 2]> = (0..frames)
            .map(|index| [(step * index as f64).sin() as f32, 0.0])
            .collect();
        DecodedAudio::new(
            SampleRate::from_hertz(rate).unwrap_or(SampleRate::MODEL),
            StereoFrames::new(samples),
        )
    }

    /// Largest deviation of the left channel from an ideal sine at the model
    /// rate, and the peak of the right channel. Both skip `margin` frames at
    /// each end, where the block transform cannot see past the clip.
    fn deviation(audio: &ModelAudio, frequency: f64, margin: usize) -> Option<(f32, f32)> {
        let frames = audio.frames().as_slice();
        let reference = tone(44_100, frequency, frames.len());
        let end = frames.len().checked_sub(margin)?;
        let actual = frames.get(margin..end)?;
        let expected = reference.frames().as_slice().get(margin..end)?;
        let left = actual
            .iter()
            .zip(expected)
            .fold(0.0_f32, |worst, (a, e)| worst.max((a[0] - e[0]).abs()));
        let right = actual
            .iter()
            .fold(0.0_f32, |worst, frame| worst.max(frame[1].abs()));
        Some((left, right))
    }

    #[test]
    fn model_rate_audio_passes_through_untouched() -> Result<(), ResampleError> {
        let audio = tone(44_100, 440.0, 1000);
        let expected = audio.frames().clone();

        assert_eq!(resample(audio)?.into_frames(), expected);
        Ok(())
    }

    #[test]
    fn duration_is_preserved() -> Result<(), ResampleError> {
        // One second and a bit, so that the length is not a multiple of a block.
        for (rate, frames, expected) in [
            (48_000, 48_000 + 123, 44_214),
            (22_050, 22_050 + 7, 44_114),
            (32_000, 32_000, 44_100),
            (96_000, 96_000 + 1, 44_101),
            (8_000, 8_000, 44_100),
        ] {
            let output = resample(tone(rate, 440.0, frames))?;
            assert_eq!(
                output.frames().len().get(),
                expected,
                "from {rate} Hz, {frames} frames"
            );
        }
        Ok(())
    }

    #[test]
    fn downsampled_tone_keeps_frequency_phase_and_level() -> Result<(), Box<dyn std::error::Error>>
    {
        let output = resample(tone(48_000, 1000.0, 96_000))?;

        let (left, right) = deviation(&output, 1000.0, 4096).ok_or("clip too short")?;
        assert!(left < 1e-4, "left deviates by {left}");
        assert!(right < 1e-6, "right channel leaks {right}");
        Ok(())
    }

    #[test]
    fn upsampled_tone_keeps_frequency_phase_and_level() -> Result<(), Box<dyn std::error::Error>> {
        let output = resample(tone(22_050, 1000.0, 44_100))?;

        let (left, right) = deviation(&output, 1000.0, 4096).ok_or("clip too short")?;
        assert!(left < 1e-4, "left deviates by {left}");
        assert!(right < 1e-6, "right channel leaks {right}");
        Ok(())
    }

    #[test]
    fn content_above_the_new_nyquist_is_removed() -> Result<(), Box<dyn std::error::Error>> {
        // 30 kHz exists at 96 kHz but not at 44.1 kHz; left in, it would
        // alias down to 14.1 kHz at full level.
        let output = resample(tone(96_000, 30_000.0, 96_000))?;

        let frames = output.frames().as_slice();
        let interior = frames
            .get(4096..frames.len() - 4096)
            .ok_or("clip too short")?;
        let peak = interior
            .iter()
            .fold(0.0_f32, |worst, frame| worst.max(frame[0].abs()));
        assert!(peak < 1e-5, "aliased level {peak}");
        Ok(())
    }

    #[test]
    fn top_of_the_audible_band_survives() -> Result<(), Box<dyn std::error::Error>> {
        for frequency in [15_000.0, 18_000.0, 20_000.0] {
            let output = resample(tone(48_000, frequency, 96_000))?;

            let (left, _) = deviation(&output, frequency, 4096).ok_or("clip too short")?;
            assert!(left < 1e-4, "{frequency} Hz deviates by {left}");
        }
        Ok(())
    }

    #[test]
    fn short_and_empty_clips_are_handled() -> Result<(), ResampleError> {
        assert_eq!(resample(tone(48_000, 440.0, 0))?.frames().len().get(), 0);
        assert_eq!(resample(tone(48_000, 440.0, 1))?.frames().len().get(), 1);
        assert_eq!(
            resample(tone(48_000, 440.0, 160))?.frames().len().get(),
            147
        );
        Ok(())
    }
}
