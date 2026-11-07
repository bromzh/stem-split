//! One pass of a separator over a whole track.
use std::num::NonZeroUsize;

use super::{
    ActiveDevice, InferenceError, Separator,
    overlap::OverlapAdd,
    plan::{ChunkPlan, Normalization},
};
use crate::{audio::WriteError, cancel::CancelToken};

pub use stem_split_audio::FrameSink;

impl std::fmt::Debug for Target<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Target")
            .field("stem", &self.stem)
            .field("extra_stems", &self.extra_stems)
            .finish_non_exhaustive()
    }
}

/// One output stream a pass produces: where its stems are in the model's
/// output and where their summed frames go. A single stem is a sum of one.
pub struct Target<'a> {
    /// Position of the first stem in the model's output.
    pub stem: usize,
    /// The other stems that go into the same stream, for a mixed output.
    pub extra_stems: &'a [usize],
    pub sink: &'a mut dyn FrameSink,
}

#[derive(Debug, thiserror::Error)]
pub enum PassError {
    #[error(transparent)]
    Inference(#[from] InferenceError),
    #[error(transparent)]
    Write(#[from] WriteError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassEnd {
    Completed,
    Cancelled,
}

/// The audio a pass works on and how it is cut up. Shared by every pass of a
/// job, so that all stems are blended identically.
#[derive(Debug, Clone, Copy)]
pub struct Track<'a> {
    pub frames: &'a [[f32; 2]],
    pub plan: &'a ChunkPlan,
    pub normalization: Normalization,
}

/// Runs `separator` over every chunk of `track` and streams the stems named
/// by `targets` to their sinks.
///
/// `cancel` is checked before each chunk, so a cancelled pass stops within
/// one model run. `on_chunk` is called after each chunk with the device that
/// computed it.
/// # Errors
/// Returns an inference or output error from the pass.
pub fn run_pass(
    track: Track<'_>,
    separator: &mut dyn Separator,
    targets: &mut [Target<'_>],
    on_chunk: &mut dyn FnMut(ActiveDevice),
    cancel: &CancelToken,
) -> Result<PassEnd, PassError> {
    let mut blends: Vec<OverlapAdd> = targets.iter().map(|_| OverlapAdd::new()).collect();
    // Scratch for targets that sum several stems. Blending is linear, so the
    // stems of a chunk can be summed before the window is applied.
    let mut sum = (Vec::new(), Vec::new());
    if targets.iter().any(|target| !target.extra_stems.is_empty()) {
        sum.0.resize(super::SEGMENT_FRAMES, 0.0);
        sum.1.resize(super::SEGMENT_FRAMES, 0.0);
    }

    for chunk in track.plan.chunks() {
        if cancel.is_cancelled() {
            return Ok(PassEnd::Cancelled);
        }
        let segment = chunk.segment(track.frames, track.normalization);
        let device = separator.device();
        let output = separator.separate(&segment)?;
        let stem = |index: usize| {
            output.stem(index).ok_or(InferenceError::MissingStem {
                index,
                available: output.stem_count().get(),
            })
        };

        for (target, blend) in targets.iter_mut().zip(&mut blends) {
            let (left, right) = stem(target.stem)?;
            let (left, right) = if target.extra_stems.is_empty() {
                (left, right)
            } else {
                let (sum_left, sum_right) = &mut sum;
                sum_left.copy_from_slice(left);
                sum_right.copy_from_slice(right);
                for &extra in target.extra_stems {
                    let (extra_left, extra_right) = stem(extra)?;
                    for (total, &value) in sum_left.iter_mut().zip(extra_left) {
                        *total += value;
                    }
                    for (total, &value) in sum_right.iter_mut().zip(extra_right) {
                        *total += value;
                    }
                }
                (sum_left.as_slice(), sum_right.as_slice())
            };
            blend.add(chunk, track.plan.window(), left, right);

            let mut settled = blend.take_until(chunk.settled());
            let parts = NonZeroUsize::MIN.saturating_add(target.extra_stems.len());
            for sample in settled.as_flattened_mut() {
                *sample = track.normalization.undo_sum(*sample, parts);
            }
            target.sink.write(&settled)?;
        }
        on_chunk(device);
    }
    Ok(PassEnd::Completed)
}

#[cfg(test)]
mod tests {
    use std::{error::Error, num::NonZeroUsize};

    use super::*;
    use crate::algorithm::{SEGMENT_FRAMES, testing::ScalingSeparator};

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
                    stem: 0,
                    extra_stems: &[],
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
                    stem: 2,
                    extra_stems: &[],
                    sink: &mut third,
                },
                Target {
                    stem: 0,
                    extra_stems: &[],
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
                stem: 0,
                extra_stems: &[],
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
                stem: 0,
                extra_stems: &[],
                sink: &mut output,
            }],
            &mut |_| {},
            &cancel,
        )?;

        assert_eq!(end, PassEnd::Cancelled);
        assert_eq!(separator.calls, 0);
        assert_eq!(output, Vec::<[f32; 2]>::new());
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
                stem: 4,
                extra_stems: &[],
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

    #[test]
    fn a_target_can_sum_several_stems() -> Result<(), Box<dyn Error>> {
        let track = 400_000;
        let frames = signal(track);
        let plan = ChunkPlan::new(NonZeroUsize::new(track).ok_or("empty")?);
        // The scaling separator returns a mathematical gain, so an identity
        // normalization keeps the expected sum straightforward.
        let normalization = Normalization::of(&[]);
        let mut separator = ScalingSeparator::new(ONE.saturating_add(3));
        let mut mixed: Vec<[f32; 2]> = Vec::new();
        let mut second: Vec<[f32; 2]> = Vec::new();

        run_pass(
            Track {
                frames: &frames,
                plan: &plan,
                normalization,
            },
            &mut separator,
            &mut [
                Target {
                    stem: 0,
                    extra_stems: &[1, 3],
                    sink: &mut mixed,
                },
                Target {
                    stem: 1,
                    extra_stems: &[],
                    sink: &mut second,
                },
            ],
            &mut |_| {},
            &CancelToken::new(),
        )?;

        // Stem `k` of the test separator is the input times `k + 1`, so the
        // mix of stems 0, 1 and 3 is the input times 1 + 2 + 4.
        assert_eq!(mixed.len(), track);
        assert_eq!(second.len(), track);
        assert!(worst_error(&mixed, &frames, 7.0) < 1e-5);
        assert!(worst_error(&second, &frames, 2.0) < 1e-5);
        Ok(())
    }

    #[test]
    fn a_missing_extra_stem_is_an_error() -> Result<(), Box<dyn Error>> {
        let frames = signal(1000);
        let plan = ChunkPlan::new(NonZeroUsize::new(1000).ok_or("empty")?);
        let mut separator = ScalingSeparator::new(ONE);
        let mut output: Vec<[f32; 2]> = Vec::new();

        let result = run_pass(
            Track {
                frames: &frames,
                plan: &plan,
                normalization: Normalization::of(&[]),
            },
            &mut separator,
            &mut [Target {
                stem: 0,
                extra_stems: &[2],
                sink: &mut output,
            }],
            &mut |_| {},
            &CancelToken::new(),
        );

        assert!(matches!(
            result,
            Err(PassError::Inference(InferenceError::MissingStem {
                index: 2,
                available: 1
            }))
        ));
        Ok(())
    }
}
