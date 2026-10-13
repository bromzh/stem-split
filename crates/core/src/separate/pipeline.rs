//! One pass of a separator over a whole track.

use super::{
    ActiveDevice, InferenceError, Separator,
    overlap::OverlapAdd,
    plan::{ChunkPlan, Normalization},
};
use crate::{
    audio::{WavWriter, WriteError},
    cancel::CancelToken,
};

/// Receives the finished frames of one stem, in track order.
pub(crate) trait FrameSink {
    fn write(&mut self, frames: &[[f32; 2]]) -> Result<(), WriteError>;
}

impl FrameSink for WavWriter {
    fn write(&mut self, frames: &[[f32; 2]]) -> Result<(), WriteError> {
        Self::write(self, frames)
    }
}

/// One stem a pass produces: where it is in the model's output and where its
/// frames go.
pub(crate) struct Target<'a> {
    pub(crate) stem_index: usize,
    pub(crate) sink: &'a mut dyn FrameSink,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PassError {
    #[error(transparent)]
    Inference(#[from] InferenceError),
    #[error(transparent)]
    Write(#[from] WriteError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassEnd {
    Completed,
    Cancelled,
}

/// The audio a pass works on and how it is cut up. Shared by every pass of a
/// job, so that all stems are blended identically.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Track<'a> {
    pub(crate) frames: &'a [[f32; 2]],
    pub(crate) plan: &'a ChunkPlan,
    pub(crate) normalization: Normalization,
}

/// Runs `separator` over every chunk of `track` and streams the stems named
/// by `targets` to their sinks.
///
/// `cancel` is checked before each chunk, so a cancelled pass stops within
/// one model run. `on_chunk` is called after each chunk with the device that
/// computed it.
pub(crate) fn run_pass(
    track: Track<'_>,
    separator: &mut dyn Separator,
    targets: &mut [Target<'_>],
    on_chunk: &mut dyn FnMut(ActiveDevice),
    cancel: &CancelToken,
) -> Result<PassEnd, PassError> {
    let mut blends: Vec<OverlapAdd> = targets.iter().map(|_| OverlapAdd::new()).collect();

    for chunk in track.plan.chunks() {
        if cancel.is_cancelled() {
            return Ok(PassEnd::Cancelled);
        }
        let segment = chunk.segment(track.frames, track.normalization);
        let device = separator.device();
        let output = separator.separate(&segment)?;

        for (target, blend) in targets.iter_mut().zip(&mut blends) {
            let (left, right) =
                output
                    .stem(target.stem_index)
                    .ok_or(InferenceError::MissingStem {
                        index: target.stem_index,
                        available: output.stem_count().get(),
                    })?;
            blend.add(chunk, track.plan.window(), left, right);

            let mut settled = blend.take_until(chunk.settled());
            for sample in settled.as_flattened_mut() {
                *sample = track.normalization.undo(*sample);
            }
            target.sink.write(&settled)?;
        }
        on_chunk(device);
    }
    Ok(PassEnd::Completed)
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// Collects frames in memory.
    impl FrameSink for Vec<[f32; 2]> {
        fn write(&mut self, frames: &[[f32; 2]]) -> Result<(), WriteError> {
            self.extend_from_slice(frames);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, num::NonZeroUsize};

    use super::*;
    use crate::separate::{SEGMENT_FRAMES, testing::ScalingSeparator};

    const ONE: NonZeroUsize = NonZeroUsize::MIN;

    /// Decaying noise-like signal with a DC offset, so that normalization has
    /// both a mean and a scale to undo.
    fn signal(track: usize) -> Vec<[f32; 2]> {
        let mut state = 0x2545_f491_u32;
        (0..track)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let [_, _, low, high] = state.to_le_bytes();
                let noise = f32::from(u16::from_le_bytes([low, high])) / 32_768.0 - 1.0;
                [0.3 * noise + 0.1, -0.2 * noise + 0.1]
            })
            .collect()
    }

    fn worst_error(actual: &[[f32; 2]], expected: &[[f32; 2]], gain: f32) -> f32 {
        actual
            .iter()
            .zip(expected)
            .flat_map(|(a, e)| a.iter().zip(e))
            .fold(0.0_f32, |worst, (a, e)| worst.max((a - e * gain).abs()))
    }

    #[test]
    fn identity_separator_returns_the_input() -> Result<(), Box<dyn Error>> {
        for track in [500, SEGMENT_FRAMES + 12_345, 700_000] {
            let frames = signal(track);
            let plan = ChunkPlan::new(NonZeroUsize::new(track).ok_or("empty")?);
            let mut separator = ScalingSeparator::new(ONE);
            let mut output: Vec<[f32; 2]> = Vec::new();
            let mut chunks = 0;

            let end = run_pass(
                Track {
                    frames: &frames,
                    plan: &plan,
                    normalization: Normalization::of(&frames),
                },
                &mut separator,
                &mut [Target {
                    stem_index: 0,
                    sink: &mut output,
                }],
                &mut |_| chunks += 1,
                &CancelToken::new(),
            )?;

            assert_eq!(end, PassEnd::Completed);
            assert_eq!(chunks, plan.count().get());
            assert_eq!(separator.calls, plan.count().get());
            assert_eq!(output.len(), track);
            let error = worst_error(&output, &frames, 1.0);
            assert!(error < 1e-5, "track of {track} frames: error {error}");
        }
        Ok(())
    }

    #[test]
    fn targets_receive_the_stem_they_name() -> Result<(), Box<dyn Error>> {
        let track = 400_000;
        let frames = signal(track);
        let plan = ChunkPlan::new(NonZeroUsize::new(track).ok_or("empty")?);
        // No offset: a scaled stem is then simply the scaled input.
        let normalization = Normalization::of(&[]);
        let mut separator = ScalingSeparator::new(ONE.saturating_add(3));
        let mut third: Vec<[f32; 2]> = Vec::new();
        let mut first: Vec<[f32; 2]> = Vec::new();

        run_pass(
            Track {
                frames: &frames,
                plan: &plan,
                normalization,
            },
            &mut separator,
            &mut [
                Target {
                    stem_index: 2,
                    sink: &mut third,
                },
                Target {
                    stem_index: 0,
                    sink: &mut first,
                },
            ],
            &mut |_| {},
            &CancelToken::new(),
        )?;

        assert!(worst_error(&third, &frames, 3.0) < 1e-5);
        assert!(worst_error(&first, &frames, 1.0) < 1e-5);
        assert_eq!(third.len(), track);
        assert_eq!(first.len(), track);
        Ok(())
    }

    #[test]
    fn cancelled_pass_stops_before_the_next_chunk() -> Result<(), Box<dyn Error>> {
        let track = 2_000_000;
        let frames = signal(track);
        let plan = ChunkPlan::new(NonZeroUsize::new(track).ok_or("empty")?);
        let mut separator = ScalingSeparator::new(ONE);
        let mut output: Vec<[f32; 2]> = Vec::new();
        let cancel = CancelToken::new();

        let mut chunks = 0;
        let end = run_pass(
            Track {
                frames: &frames,
                plan: &plan,
                normalization: Normalization::of(&frames),
            },
            &mut separator,
            &mut [Target {
                stem_index: 0,
                sink: &mut output,
            }],
            &mut |_| {
                chunks += 1;
                if chunks == 2 {
                    cancel.cancel();
                }
            },
            &cancel,
        )?;

        assert_eq!(end, PassEnd::Cancelled);
        assert!(plan.count().get() > 2);
        assert_eq!(separator.calls, 2, "no model run after cancellation");
        assert!(output.len() < track);
        Ok(())
    }

    #[test]
    fn already_cancelled_pass_does_no_work() -> Result<(), Box<dyn Error>> {
        let frames = signal(1000);
        let plan = ChunkPlan::new(NonZeroUsize::new(1000).ok_or("empty")?);
        let mut separator = ScalingSeparator::new(ONE);
        let mut output: Vec<[f32; 2]> = Vec::new();
        let cancel = CancelToken::new();
        cancel.cancel();

        let end = run_pass(
            Track {
                frames: &frames,
                plan: &plan,
                normalization: Normalization::of(&frames),
            },
            &mut separator,
            &mut [Target {
                stem_index: 0,
                sink: &mut output,
            }],
            &mut |_| {},
            &cancel,
        )?;

        assert_eq!(end, PassEnd::Cancelled);
        assert_eq!(separator.calls, 0);
        assert!(output.is_empty());
        Ok(())
    }

    #[test]
    fn stem_outside_the_model_output_is_an_error() -> Result<(), Box<dyn Error>> {
        let frames = signal(1000);
        let plan = ChunkPlan::new(NonZeroUsize::new(1000).ok_or("empty")?);
        let mut separator = ScalingSeparator::new(ONE);
        let mut output: Vec<[f32; 2]> = Vec::new();

        let result = run_pass(
            Track {
                frames: &frames,
                plan: &plan,
                normalization: Normalization::of(&frames),
            },
            &mut separator,
            &mut [Target {
                stem_index: 4,
                sink: &mut output,
            }],
            &mut |_| {},
            &CancelToken::new(),
        );

        assert!(matches!(
            result,
            Err(PassError::Inference(InferenceError::MissingStem {
                index: 4,
                available: 1
            }))
        ));
        Ok(())
    }
}
