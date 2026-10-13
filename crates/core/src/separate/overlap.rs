//! Blending the results of overlapping chunks back into one stem.

use super::plan::Chunk;

/// Weighted overlap-add for one stem.
///
/// Chunks are added in track order. Each adds its frames multiplied by the
/// blending window; where two chunks overlap, the result is their weighted
/// average. Frames that no later chunk can reach are handed out and
/// forgotten, so only about a segment of audio is held at any time.
#[derive(Debug, Default)]
pub(crate) struct OverlapAdd {
    /// Track position of the first frame still held.
    start: usize,
    /// Weighted sums of the frames held.
    sums: Vec<[f32; 2]>,
    /// Sum of the weights each held frame has received.
    weights: Vec<f32>,
}

impl OverlapAdd {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Adds what one chunk contributes: `left` and `right` are the model's
    /// output for the whole window of `chunk`, `window` the blending weights.
    pub(crate) fn add(&mut self, chunk: &Chunk, window: &[f32], left: &[f32], right: &[f32]) {
        let begin = chunk.offset().saturating_sub(self.start);
        let end = begin.saturating_add(chunk.length());
        if self.sums.len() < end {
            self.sums.resize(end, [0.0; 2]);
            self.weights.resize(end, 0.0);
        }

        let held = self
            .sums
            .iter_mut()
            .zip(self.weights.iter_mut())
            .skip(begin);
        // The chunk's own frames start `lead` frames into the model window.
        let contribution = left
            .iter()
            .zip(right)
            .skip(chunk.lead())
            .zip(window)
            .take(chunk.length());
        for (([sum_left, sum_right], total), ((&left, &right), &weight)) in held.zip(contribution) {
            *sum_left += left * weight;
            *sum_right += right * weight;
            *total += weight;
        }
    }

    /// Hands out the frames before track position `boundary`, averaged by
    /// their weights. The caller passes a position no later chunk reaches.
    pub(crate) fn take_until(&mut self, boundary: usize) -> Vec<[f32; 2]> {
        let count = boundary.saturating_sub(self.start).min(self.sums.len());
        self.start = self.start.saturating_add(count);
        self.sums
            .drain(..count)
            .zip(self.weights.drain(..count))
            .map(|([left, right], weight)| {
                // Every frame of a chunk has a positive weight. A frame no
                // chunk covered would have none; it stays silent.
                if weight > 0.0 {
                    [left / weight, right / weight]
                } else {
                    [0.0; 2]
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::*;
    use crate::separate::{SEGMENT_FRAMES, plan::ChunkPlan};

    /// A track-long ramp, so that every frame has a distinct value.
    #[expect(clippy::cast_precision_loss, reason = "test ramp of small integers")]
    fn ramp(track: usize) -> Vec<[f32; 2]> {
        (0..track)
            .map(|index| {
                let value = (index % 1000) as f32 / 1000.0;
                [value, -value]
            })
            .collect()
    }

    /// Runs the blending with a "model" that returns its input unchanged.
    fn blend_identity(track: usize) -> Result<Vec<[f32; 2]>, &'static str> {
        let frames = ramp(track);
        let plan = ChunkPlan::new(NonZeroUsize::new(track).ok_or("empty track")?);
        let normalization = crate::separate::plan::Normalization::of(&[]);
        let mut blend = OverlapAdd::new();
        let mut output = Vec::new();
        for chunk in plan.chunks() {
            let segment = chunk.segment(&frames, normalization);
            blend.add(chunk, plan.window(), segment.left(), segment.right());
            output.extend(blend.take_until(chunk.settled()));
        }
        Ok(output)
    }

    #[test]
    fn identity_model_reproduces_the_track() -> Result<(), &'static str> {
        for track in [
            1,
            1000,
            SEGMENT_FRAMES - 1,
            SEGMENT_FRAMES,
            SEGMENT_FRAMES + 1,
            1_000_000,
        ] {
            let output = blend_identity(track)?;
            let expected = ramp(track);

            assert_eq!(output.len(), track, "track of {track} frames");
            let worst = output
                .iter()
                .zip(&expected)
                .flat_map(|(actual, wanted)| actual.iter().zip(wanted))
                .fold(0.0_f32, |worst, (a, e)| worst.max((a - e).abs()));
            assert!(worst < 1e-5, "track of {track} frames: error {worst}");
        }
        Ok(())
    }

    #[test]
    fn frames_are_released_as_soon_as_they_are_settled() -> Result<(), &'static str> {
        let track = 1_000_000;
        let frames = ramp(track);
        let plan = ChunkPlan::new(NonZeroUsize::new(track).ok_or("empty track")?);
        let normalization = crate::separate::plan::Normalization::of(&[]);
        let mut blend = OverlapAdd::new();

        let mut released = 0;
        for chunk in plan.chunks() {
            let segment = chunk.segment(&frames, normalization);
            blend.add(chunk, plan.window(), segment.left(), segment.right());
            released += blend.take_until(chunk.settled()).len();

            assert_eq!(released, chunk.settled());
            // What stays behind is the part the next chunk still overlaps.
            assert!(blend.sums.len() <= SEGMENT_FRAMES / 4);
        }
        assert_eq!(released, track);
        assert!(blend.sums.is_empty());
        Ok(())
    }

    #[test]
    fn overlapping_chunks_are_averaged_by_weight() -> Result<(), &'static str> {
        // Two chunks that disagree: the first says 1, the second says 3.
        let track = SEGMENT_FRAMES;
        let plan = ChunkPlan::new(NonZeroUsize::new(track).ok_or("empty track")?);
        let ones = vec![1.0_f32; SEGMENT_FRAMES];
        let threes = vec![3.0_f32; SEGMENT_FRAMES];
        let mut blend = OverlapAdd::new();
        let mut output = Vec::new();
        for (chunk, values) in plan.chunks().zip([&ones, &threes]) {
            blend.add(chunk, plan.window(), values, values);
            output.extend(blend.take_until(chunk.settled()));
        }

        let stride = SEGMENT_FRAMES / 4 * 3;
        // Before the overlap only the first chunk speaks.
        assert_eq!(output.get(stride - 1).copied(), Some([1.0, 1.0]));
        // Inside it the answer moves from the first towards the second.
        let early = output.get(stride).ok_or("overlap start")?;
        let late = output.last().ok_or("overlap end")?;
        assert!(early[0] > 1.0 && early[0] < 1.01, "{early:?}");
        assert!(late[0] > 2.99 && late[0] < 3.0, "{late:?}");
        assert!(output.windows(2).all(|pair| match pair {
            [before, after] => before[0] <= after[0],
            _ => true,
        }));
        Ok(())
    }

    #[test]
    fn taking_past_the_held_frames_returns_only_what_exists() {
        let mut blend = OverlapAdd::new();
        assert!(blend.take_until(1000).is_empty());
        assert!(blend.take_until(0).is_empty());
    }
}
