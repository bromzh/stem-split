//! Cutting a track into overlapping segments, following `apply_model` of the
//! reference Demucs implementation so that results can be compared with it.

use std::{iter, num::NonZeroUsize};

use super::{SEGMENT_FRAMES, Segment};

/// Distance between the starts of consecutive chunks: neighbours overlap by a
/// quarter of a segment.
pub(crate) const STRIDE_FRAMES: usize = SEGMENT_FRAMES / 4 * 3;

/// Below this deviation a track counts as silent and is left unscaled, which
/// avoids dividing by zero.
const SILENCE_DEVIATION: f64 = 1e-8;

#[expect(
    clippy::cast_precision_loss,
    reason = "frame counts are far below 2^52"
)]
const fn count_as_f64(count: usize) -> f64 {
    count as f64
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "audio statistics are within f32 range"
)]
const fn narrow(value: f64) -> f32 {
    value as f32
}

/// Offset and scale that bring a track to zero mean and unit deviation, the
/// form the models were trained on. Statistics come from the mono downmix of
/// the whole track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Normalization {
    mean: f32,
    deviation: f32,
}

impl Normalization {
    pub(crate) fn of(frames: &[[f32; 2]]) -> Self {
        let mono = || {
            frames
                .iter()
                .map(|&[left, right]| f64::midpoint(f64::from(left), f64::from(right)))
        };
        let count = frames.len();
        if count == 0 {
            return Self {
                mean: 0.0,
                deviation: 1.0,
            };
        }
        let mean = mono().sum::<f64>() / count_as_f64(count);
        // The unbiased estimate, as `torch.std` computes it.
        let variance = mono().map(|sample| (sample - mean).powi(2)).sum::<f64>()
            / count_as_f64(count.saturating_sub(1).max(1));
        let deviation = variance.sqrt();
        Self {
            mean: narrow(mean),
            deviation: if deviation < SILENCE_DEVIATION {
                1.0
            } else {
                narrow(deviation)
            },
        }
    }

    pub(crate) fn apply(self, sample: f32) -> f32 {
        (sample - self.mean) / self.deviation
    }

    pub(crate) fn undo(self, sample: f32) -> f32 {
        sample * self.deviation + self.mean
    }
}

/// The part of a track one model run is responsible for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Chunk {
    /// Track position of the first frame the chunk contributes.
    offset: usize,
    /// Frames it contributes: a full segment, or what is left at the end.
    length: usize,
    /// Frames of the model window that precede `offset`. Zero for a full
    /// chunk; a short last chunk sits in the middle of its window, with real
    /// audio before it and silence after the end of the track.
    lead: usize,
    /// Track position up to which no later chunk contributes.
    settled: usize,
}

impl Chunk {
    fn at(offset: usize, track_frames: usize) -> Self {
        let length = SEGMENT_FRAMES.min(track_frames.saturating_sub(offset));
        Self {
            offset,
            length,
            lead: (SEGMENT_FRAMES - length) / 2,
            settled: offset.saturating_add(STRIDE_FRAMES).min(track_frames),
        }
    }

    pub(crate) const fn offset(&self) -> usize {
        self.offset
    }

    pub(crate) const fn length(&self) -> usize {
        self.length
    }

    pub(crate) const fn lead(&self) -> usize {
        self.lead
    }

    pub(crate) const fn settled(&self) -> usize {
        self.settled
    }

    /// The model input for this chunk: normalized audio, silent wherever the
    /// window reaches outside the track.
    pub(crate) fn segment(&self, frames: &[[f32; 2]], normalization: Normalization) -> Segment {
        // The window starts `lead` frames before the chunk. Where that is
        // before the start of the track, the segment stays silent instead.
        let silent_start = self.lead.saturating_sub(self.offset);
        let source_start = self.offset.saturating_sub(self.lead);
        let source = frames.get(source_start..).unwrap_or(&[]);

        let mut segment = Segment::silent();
        let (left, right) = segment.channels_mut();
        let slots = left.iter_mut().zip(right.iter_mut()).skip(silent_start);
        for ((left, right), &[source_left, source_right]) in slots.zip(source) {
            *left = normalization.apply(source_left);
            *right = normalization.apply(source_right);
        }
        segment
    }
}

/// The chunks of a track and the weights their results are blended with.
/// Never empty: a track of at least one frame has at least one chunk.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChunkPlan {
    first: Chunk,
    rest: Vec<Chunk>,
    window: Vec<f32>,
}

impl ChunkPlan {
    pub(crate) fn new(track_frames: NonZeroUsize) -> Self {
        let track_frames = track_frames.get();
        Self {
            first: Chunk::at(0, track_frames),
            rest: (STRIDE_FRAMES..track_frames)
                .step_by(STRIDE_FRAMES)
                .map(|offset| Chunk::at(offset, track_frames))
                .collect(),
            window: triangular_window(),
        }
    }

    pub(crate) fn chunks(&self) -> impl Iterator<Item = &Chunk> {
        iter::once(&self.first).chain(&self.rest)
    }

    pub(crate) const fn count(&self) -> NonZeroUsize {
        NonZeroUsize::MIN.saturating_add(self.rest.len())
    }

    /// Blending weights for the frames of a chunk, counted from its first
    /// frame: a triangle that peaks in the middle of a segment.
    pub(crate) fn window(&self) -> &[f32] {
        &self.window
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "weights count up to half a segment, well below 2^24"
)]
fn triangular_window() -> Vec<f32> {
    let half = SEGMENT_FRAMES / 2;
    let peak = half as f32;
    (1..=half)
        .chain((1..=SEGMENT_FRAMES - half).rev())
        .map(|step| step as f32 / peak)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(track_frames: usize) -> Result<ChunkPlan, &'static str> {
        NonZeroUsize::new(track_frames)
            .map(ChunkPlan::new)
            .ok_or("empty track")
    }

    fn offsets(plan: &ChunkPlan) -> Vec<(usize, usize)> {
        plan.chunks()
            .map(|chunk| (chunk.offset(), chunk.length()))
            .collect()
    }

    #[test]
    fn stride_is_three_quarters_of_a_segment() {
        assert_eq!(STRIDE_FRAMES, 257_985);
    }

    #[test]
    fn track_shorter_than_a_segment_is_one_centred_chunk() -> Result<(), &'static str> {
        let plan = plan(1000)?;

        assert_eq!(offsets(&plan), [(0, 1000)]);
        assert_eq!(plan.count().get(), 1);
        let chunk = plan.chunks().next().ok_or("no chunk")?;
        assert_eq!(chunk.lead(), (SEGMENT_FRAMES - 1000) / 2);
        assert_eq!(chunk.settled(), 1000);
        Ok(())
    }

    #[test]
    fn track_of_exactly_one_segment_has_a_short_second_chunk() -> Result<(), &'static str> {
        // As in the reference: offsets step by the stride while they are
        // inside the track, so a quarter-segment tail gets its own chunk.
        let plan = plan(SEGMENT_FRAMES)?;

        assert_eq!(
            offsets(&plan),
            [
                (0, SEGMENT_FRAMES),
                (STRIDE_FRAMES, SEGMENT_FRAMES - STRIDE_FRAMES)
            ]
        );
        Ok(())
    }

    #[test]
    fn chunks_step_by_the_stride_and_cover_the_track() -> Result<(), &'static str> {
        let track = 1_000_000;
        let plan = plan(track)?;

        assert_eq!(
            offsets(&plan),
            [
                (0, SEGMENT_FRAMES),
                (257_985, SEGMENT_FRAMES),
                (515_970, SEGMENT_FRAMES),
                (773_955, track - 773_955),
            ]
        );
        assert_eq!(plan.count().get(), 4);
        // Each chunk settles the frames up to where the next one starts.
        let settled: Vec<usize> = plan.chunks().map(Chunk::settled).collect();
        assert_eq!(settled, [257_985, 515_970, 773_955, track]);
        Ok(())
    }

    #[test]
    fn four_minute_track_has_42_chunks() -> Result<(), &'static str> {
        assert_eq!(plan(240 * 44_100)?.count().get(), 42);
        Ok(())
    }

    #[test]
    fn full_chunk_starts_its_window_at_its_offset() -> Result<(), &'static str> {
        let plan = plan(1_000_000)?;
        let chunk = plan.chunks().nth(1).ok_or("no second chunk")?;

        assert_eq!(chunk.lead(), 0);
        Ok(())
    }

    #[test]
    fn window_is_a_triangle_peaking_at_the_middle() -> Result<(), &'static str> {
        let plan = plan(1)?;
        let window = plan.window();
        let half = SEGMENT_FRAMES / 2;

        assert_eq!(window.len(), SEGMENT_FRAMES);
        assert_eq!(window.first().copied(), Some(1.0 / 171_990.0));
        assert_eq!(window.get(half - 1).copied(), Some(1.0));
        assert_eq!(window.get(half).copied(), Some(1.0));
        assert_eq!(window.last().copied(), Some(1.0 / 171_990.0));
        assert!(window.iter().all(|&weight| weight > 0.0 && weight <= 1.0));
        Ok(())
    }

    #[test]
    fn normalization_brings_the_downmix_to_zero_mean_unit_deviation() {
        // Mono downmix: 1, 3, 5, 7. Mean 4, unbiased deviation sqrt(20 / 3).
        let frames = [[0.0, 2.0], [2.0, 4.0], [4.0, 6.0], [6.0, 8.0]];
        let normalization = Normalization::of(&frames);
        let deviation = (20.0_f32 / 3.0).sqrt();

        assert!((normalization.apply(4.0)).abs() < 1e-6);
        assert!((normalization.apply(4.0 + deviation) - 1.0).abs() < 1e-6);
        for sample in [-3.5_f32, 0.0, 0.25, 9.0] {
            let restored = normalization.undo(normalization.apply(sample));
            assert!((restored - sample).abs() < 1e-5, "{sample} -> {restored}");
        }
    }

    #[test]
    fn silent_and_empty_tracks_are_left_unscaled() {
        for frames in [vec![], vec![[0.0, 0.0]; 100], vec![[0.25, 0.25]; 100]] {
            let normalization = Normalization::of(&frames);
            let level = frames.first().map_or(0.0, |&[left, _]| left);
            // A constant track only has its offset removed.
            assert!((normalization.apply(level)).abs() < 1e-6);
            assert!((normalization.apply(level + 1.0) - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn full_chunk_segment_is_the_normalized_audio_at_its_offset() -> Result<(), &'static str> {
        let track = SEGMENT_FRAMES + 10;
        #[expect(clippy::cast_precision_loss, reason = "test ramp of small integers")]
        let frames: Vec<[f32; 2]> = (0..track)
            .map(|index| [index as f32, -(index as f32)])
            .collect();
        let normalization = Normalization {
            mean: 0.0,
            deviation: 2.0,
        };
        let plan = plan(track)?;
        let first = plan.chunks().next().ok_or("no chunk")?;

        let segment = first.segment(&frames, normalization);

        assert_eq!(segment.left().first().copied(), Some(0.0));
        assert_eq!(segment.left().get(100).copied(), Some(50.0));
        assert_eq!(segment.right().get(100).copied(), Some(-50.0));
        assert_eq!(
            segment.left().last().copied(),
            Some(halved(SEGMENT_FRAMES - 1))
        );
        Ok(())
    }

    #[expect(clippy::cast_precision_loss, reason = "test ramp of small integers")]
    fn halved(index: usize) -> f32 {
        index as f32 / 2.0
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the values are copied or scaled by powers of two, so they are exact"
    )]
    fn short_last_chunk_is_centred_with_real_audio_before_it() -> Result<(), &'static str> {
        let track = STRIDE_FRAMES + 1000;
        let frames = vec![[1.0_f32, 1.0]; track];
        let normalization = Normalization {
            mean: 0.0,
            deviation: 1.0,
        };
        let plan = plan(track)?;
        let last = plan.chunks().last().ok_or("no chunk")?;
        assert_eq!((last.offset(), last.length()), (STRIDE_FRAMES, 1000));
        let lead = last.lead();
        assert_eq!(lead, (SEGMENT_FRAMES - 1000) / 2);

        let segment = last.segment(&frames, normalization);

        // Real audio fills the window up to the end of the track...
        assert!(segment.left().iter().take(lead + 1000).all(|&s| s == 1.0));
        // ...and silence follows.
        assert!(segment.left().iter().skip(lead + 1000).all(|&s| s == 0.0));
        Ok(())
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the values are copied or scaled by powers of two, so they are exact"
    )]
    fn short_track_segment_is_padded_with_silence_on_both_sides() -> Result<(), &'static str> {
        let frames = vec![[1.0_f32, -1.0]; 1000];
        let normalization = Normalization {
            mean: 0.0,
            deviation: 1.0,
        };
        let plan = plan(1000)?;
        let chunk = plan.chunks().next().ok_or("no chunk")?;
        let lead = chunk.lead();

        let segment = chunk.segment(&frames, normalization);

        assert!(segment.left().iter().take(lead).all(|&s| s == 0.0));
        assert!(
            segment
                .left()
                .iter()
                .skip(lead)
                .take(1000)
                .all(|&s| s == 1.0)
        );
        assert!(
            segment
                .right()
                .iter()
                .skip(lead)
                .take(1000)
                .all(|&s| s == -1.0)
        );
        assert!(segment.left().iter().skip(lead + 1000).all(|&s| s == 0.0));
        Ok(())
    }
}
