//! Demucs-compatible STFT and inverse STFT: the host-side counterpart of
//! `_spec` and `_ispec` in `demucs/htdemucs.py`, needed because the model
//! graphs leave the complex transforms out.

use std::{fmt, sync::Arc};

use realfft::{ComplexToReal, RealFftPlanner, RealToComplex, num_complex::Complex32};

pub(crate) const N_FFT: usize = 4096;
pub(crate) const HOP: usize = N_FFT / 4;
/// Demucs drops the Nyquist bin.
pub(crate) const BINS: usize = N_FFT / 2;
/// Extra reflect padding Demucs adds so that a signal yields `ceil(len / HOP)` frames.
const PAD: usize = HOP / 2 * 3;
/// Frames `torch.stft` produces on each side of the range Demucs keeps.
const EDGE_FRAMES: usize = 2;
/// `normalized=True` in torch: `1 / sqrt(N_FFT)`.
const SCALE: f32 = 1.0 / 64.0;
/// `N_FFT` as the widest integer type that converts to `f32` losslessly.
const N_FFT_U16: u16 = 4096;

#[derive(Debug, thiserror::Error)]
pub enum StftError {
    #[error("signal of {0} samples is too short for the transform")]
    TooShort(usize),
    #[error("FFT failed: {0}")]
    Fft(#[from] realfft::FftError),
    #[error("spectrogram plane has {actual} values, expected {expected}")]
    PlaneSize { expected: usize, actual: usize },
    #[error("overlap-add buffer of {buffer} samples cannot hold {start}..{end}")]
    Range {
        buffer: usize,
        start: usize,
        end: usize,
    },
}

/// Number of spectrogram frames Demucs keeps for a signal of `length` samples.
pub(crate) const fn frames_for(length: usize) -> usize {
    length.div_ceil(HOP)
}

/// Periodic Hann window, as `torch.hann_window(N_FFT)`.
fn hann_window() -> Vec<f32> {
    (0..N_FFT_U16)
        .map(|n| {
            let phase = std::f32::consts::TAU * f32::from(n) / f32::from(N_FFT_U16);
            0.5 - 0.5 * phase.cos()
        })
        .collect()
}

/// Reflect padding without repeating the edge sample, as `F.pad(mode="reflect")`.
fn reflect_pad(signal: &[f32], left: usize, right: usize) -> Result<Vec<f32>, StftError> {
    let len = signal.len();
    let too_short = || StftError::TooShort(len);
    let head = signal.get(1..=left).ok_or_else(too_short)?;
    let tail_start = right
        .checked_add(1)
        .and_then(|span| len.checked_sub(span))
        .ok_or_else(too_short)?;
    let tail = signal
        .get(tail_start..len.saturating_sub(1))
        .ok_or_else(too_short)?;

    let mut padded = Vec::with_capacity(len.saturating_add(left).saturating_add(right));
    padded.extend(head.iter().rev());
    padded.extend_from_slice(signal);
    padded.extend(tail.iter().rev());
    Ok(padded)
}

fn check_plane(plane: &[f32], frames: usize) -> Result<(), StftError> {
    let expected = BINS * frames;
    if frames == 0 || plane.len() != expected {
        return Err(StftError::PlaneSize {
            expected,
            actual: plane.len(),
        });
    }
    Ok(())
}

/// Forward and inverse transforms with their plans and work buffers, built
/// once and reused for every segment.
///
/// A spectrogram plane holds the real or the imaginary parts of one channel,
/// laid out as `[BINS][frames]`.
pub(crate) struct Stft {
    window: Vec<f32>,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    time: Vec<f32>,
    spectrum: Vec<Complex32>,
    forward_scratch: Vec<Complex32>,
    inverse_scratch: Vec<Complex32>,
    /// Overlap-add buffer of the inverse transform.
    signal: Vec<f32>,
    /// Sum of squared windows over the frames of the inverse transform, and
    /// the frame count it was computed for.
    envelope: Vec<f32>,
    envelope_frames: usize,
}

// The FFT plans are trait objects without a `Debug` implementation.
impl fmt::Debug for Stft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Stft")
            .field("n_fft", &N_FFT)
            .field("hop", &HOP)
            .finish_non_exhaustive()
    }
}

impl Stft {
    pub(crate) fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(N_FFT);
        let inverse = planner.plan_fft_inverse(N_FFT);
        Self {
            window: hann_window(),
            time: forward.make_input_vec(),
            spectrum: forward.make_output_vec(),
            forward_scratch: forward.make_scratch_vec(),
            inverse_scratch: inverse.make_scratch_vec(),
            forward,
            inverse,
            signal: Vec::new(),
            envelope: Vec::new(),
            envelope_frames: 0,
        }
    }

    /// Writes the spectrogram of `channel` into `real` and `imaginary`.
    ///
    /// The signal is reflect-padded as Demucs does, `frames_for(len)` of the
    /// resulting frames are kept, and the Nyquist bin is dropped.
    pub(crate) fn forward(
        &mut self,
        channel: &[f32],
        real: &mut [f32],
        imaginary: &mut [f32],
    ) -> Result<(), StftError> {
        let frames = frames_for(channel.len());
        check_plane(real, frames)?;
        check_plane(imaginary, frames)?;

        let right = (PAD + frames * HOP).saturating_sub(channel.len());
        let padded = reflect_pad(channel, PAD, right)?;
        let centered = reflect_pad(&padded, N_FFT / 2, N_FFT / 2)?;

        let kept = centered
            .windows(N_FFT)
            .step_by(HOP)
            .skip(EDGE_FRAMES)
            .take(frames);
        for (frame, samples) in kept.enumerate() {
            for ((slot, &sample), &weight) in self.time.iter_mut().zip(samples).zip(&self.window) {
                *slot = sample * weight;
            }
            self.forward.process_with_scratch(
                &mut self.time,
                &mut self.spectrum,
                &mut self.forward_scratch,
            )?;
            // A plane is bin-major, so frame `frame` is every `frames`-th value.
            let column = real
                .iter_mut()
                .skip(frame)
                .step_by(frames)
                .zip(imaginary.iter_mut().skip(frame).step_by(frames));
            for ((real, imaginary), bin) in column.zip(&self.spectrum) {
                *real = bin.re * SCALE;
                *imaginary = bin.im * SCALE;
            }
        }
        Ok(())
    }

    /// Inverse transform of one channel into `output`, whose length decides
    /// how many frames the planes must hold.
    ///
    /// Demucs kept `frames` of `frames + 4` frames in the forward transform
    /// and restores the rest as silence, so the first and last `PAD` samples
    /// come back attenuated. The models are trained with that.
    pub(crate) fn inverse(
        &mut self,
        real: &[f32],
        imaginary: &[f32],
        output: &mut [f32],
    ) -> Result<(), StftError> {
        let length = output.len();
        let frames = frames_for(length);
        check_plane(real, frames)?;
        check_plane(imaginary, frames)?;

        let total_frames = frames + 2 * EDGE_FRAMES;
        let buffer_len = N_FFT + HOP * (total_frames - 1);
        let out_of_range = |start: usize, end: usize| StftError::Range {
            buffer: buffer_len,
            start,
            end,
        };
        self.prepare_envelope(frames, buffer_len)?;
        self.signal.clear();
        self.signal.resize(buffer_len, 0.0);

        for frame in 0..frames {
            let column = real
                .iter()
                .skip(frame)
                .step_by(frames)
                .zip(imaginary.iter().skip(frame).step_by(frames));
            // The planes hold `BINS` values per frame, one fewer than the
            // spectrum: the Nyquist slot is cleared separately.
            for (slot, (&real, &imaginary)) in self.spectrum.iter_mut().zip(column) {
                *slot = Complex32::new(real, imaginary);
            }
            if let Some(nyquist) = self.spectrum.last_mut() {
                *nyquist = Complex32::new(0.0, 0.0);
            }
            // A real signal has a real DC bin; the inverse transform rejects anything else.
            if let Some(dc) = self.spectrum.first_mut() {
                dc.im = 0.0;
            }
            self.inverse.process_with_scratch(
                &mut self.spectrum,
                &mut self.time,
                &mut self.inverse_scratch,
            )?;

            let start = (frame + EDGE_FRAMES) * HOP;
            let end = start + N_FFT;
            let target = self
                .signal
                .get_mut(start..end)
                .ok_or_else(|| out_of_range(start, end))?;
            for ((sum, &sample), &weight) in target.iter_mut().zip(&self.time).zip(&self.window) {
                *sum += sample * SCALE * weight;
            }
        }

        // Undo torch's centering (N_FFT / 2) and Demucs' own padding (PAD).
        let start = N_FFT / 2 + PAD;
        let end = start + length;
        let samples = self
            .signal
            .get(start..end)
            .ok_or_else(|| out_of_range(start, end))?;
        let weights = self
            .envelope
            .get(start..end)
            .ok_or_else(|| out_of_range(start, end))?;
        for ((slot, &sample), &weight) in output.iter_mut().zip(samples).zip(weights) {
            *slot = sample / weight.max(1e-11);
        }
        Ok(())
    }

    /// The zero frames torch pads on both sides still count towards the
    /// envelope, so it depends only on the number of frames.
    fn prepare_envelope(&mut self, frames: usize, buffer_len: usize) -> Result<(), StftError> {
        if self.envelope_frames == frames && self.envelope.len() == buffer_len {
            return Ok(());
        }
        self.envelope.clear();
        self.envelope.resize(buffer_len, 0.0);
        for start in (0..frames + 2 * EDGE_FRAMES).map(|frame| frame * HOP) {
            let end = start + N_FFT;
            let target = self.envelope.get_mut(start..end).ok_or(StftError::Range {
                buffer: buffer_len,
                start,
                end,
            })?;
            for (sum, &weight) in target.iter_mut().zip(&self.window) {
                *sum += weight * weight;
            }
        }
        self.envelope_frames = frames;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(length: usize) -> Vec<f32> {
        (0..length)
            .scan(0.0_f32, |phase, _| {
                *phase += 0.05;
                Some(phase.sin() + 0.3 * (*phase * 7.1).cos())
            })
            .collect()
    }

    #[test]
    fn reflect_pad_mirrors_without_repeating_the_edge() -> Result<(), StftError> {
        let padded = reflect_pad(&[1.0, 2.0, 3.0, 4.0], 2, 1)?;
        assert_eq!(padded, [3.0, 2.0, 1.0, 2.0, 3.0, 4.0, 3.0]);
        Ok(())
    }

    #[test]
    fn segment_length_gives_336_frames() {
        assert_eq!(frames_for(crate::separate::SEGMENT_FRAMES), 336);
        assert_eq!(frames_for(1024), 1);
        assert_eq!(frames_for(1025), 2);
    }

    #[test]
    fn inverse_restores_the_interior_of_the_signal() -> Result<(), StftError> {
        let length = 5 * HOP + 300;
        let signal = tone(length);
        let frames = frames_for(length);
        assert_eq!(frames, 6);
        let mut real = vec![0.0; BINS * frames];
        let mut imaginary = vec![0.0; BINS * frames];
        let mut restored = vec![0.0; length];
        let mut stft = Stft::new();

        stft.forward(&signal, &mut real, &mut imaginary)?;
        stft.inverse(&real, &imaginary, &mut restored)?;

        // The edge frames Demucs discards cost the first and last `PAD`
        // samples; in between the only lossy step is the dropped Nyquist bin.
        let interior = PAD..frames * HOP - PAD;
        let (Some(expected), Some(actual)) = (signal.get(interior.clone()), restored.get(interior))
        else {
            return Err(StftError::TooShort(length));
        };
        let worst = expected
            .iter()
            .zip(actual)
            .fold(0.0_f32, |worst, (e, a)| worst.max((e - a).abs()));
        assert!(worst < 1e-3, "max interior error {worst}");
        Ok(())
    }

    #[test]
    fn transforms_are_repeatable_with_one_instance() -> Result<(), StftError> {
        let length = 4 * HOP;
        let frames = frames_for(length);
        let mut stft = Stft::new();
        let mut planes = [
            vec![0.0; BINS * frames],
            vec![0.0; BINS * frames],
            vec![0.0; BINS * frames],
            vec![0.0; BINS * frames],
        ];
        let [first_real, first_imaginary, second_real, second_imaginary] = &mut planes;

        stft.forward(&tone(length), first_real, first_imaginary)?;
        // A different signal in between must leave no trace in the buffers.
        let other: Vec<f32> = tone(length).iter().map(|sample| -2.0 * sample).collect();
        stft.forward(&other, second_real, second_imaginary)?;
        stft.forward(&tone(length), second_real, second_imaginary)?;

        assert_eq!(first_real, second_real);
        assert_eq!(first_imaginary, second_imaginary);

        let mut first = vec![0.0; length];
        let mut second = vec![1.0; length];
        stft.inverse(first_real, first_imaginary, &mut first)?;
        stft.inverse(first_real, first_imaginary, &mut second)?;
        assert_eq!(first, second);
        Ok(())
    }

    #[test]
    fn planes_of_the_wrong_size_are_rejected() {
        let length = 4 * HOP;
        let frames = frames_for(length);
        let mut stft = Stft::new();
        let mut good = vec![0.0; BINS * frames];
        let mut short = vec![0.0; BINS * frames - 1];

        assert!(matches!(
            stft.forward(&tone(length), &mut good, &mut short),
            Err(StftError::PlaneSize { .. })
        ));
        let mut output = vec![0.0; length];
        assert!(matches!(
            stft.inverse(&short, &good, &mut output),
            Err(StftError::PlaneSize { .. })
        ));
        assert!(matches!(
            stft.inverse(&good, &good, &mut []),
            Err(StftError::PlaneSize { .. })
        ));
    }

    #[test]
    fn signal_too_short_to_pad_is_rejected() {
        let mut stft = Stft::new();
        let frames = frames_for(100);
        let mut real = vec![0.0; BINS * frames];
        let mut imaginary = vec![0.0; BINS * frames];

        assert!(matches!(
            stft.forward(&tone(100), &mut real, &mut imaginary),
            Err(StftError::TooShort(100))
        ));
    }
}
