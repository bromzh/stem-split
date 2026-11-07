//! The transforms around an MDX-Net graph: `torch.stft` and `torch.istft`
//! with `center=True`, a periodic Hann window and no normalization, the way
//! UVR calls them. The spectrogram keeps the lowest bins only; the inverse
//! restores the rest as silence.

use std::{fmt, sync::Arc};

use realfft::{ComplexToReal, RealFftPlanner, RealToComplex, num_complex::Complex32};

use super::MdxParams;
use stem_split_inference::StftError;

/// Forward and inverse transforms of one window, with their plans and work
/// buffers, built once per network.
///
/// A spectrogram plane holds the real or the imaginary parts of one channel,
/// laid out as `[bins][frames]`.
pub(super) struct MdxStft {
    n_fft: usize,
    hop: usize,
    bins: usize,
    frames: usize,
    /// Samples of one channel of a window.
    length: usize,
    window: Vec<f32>,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    /// The channel with half an FFT of reflection on either side.
    padded: Vec<f32>,
    time: Vec<f32>,
    spectrum: Vec<Complex32>,
    forward_scratch: Vec<Complex32>,
    inverse_scratch: Vec<Complex32>,
    /// Overlap-add buffer of the inverse transform, as long as `padded`.
    signal: Vec<f32>,
    /// Sum of the squared windows over all frames, for the inverse.
    envelope: Vec<f32>,
}

// The FFT plans are trait objects without a `Debug` implementation.
impl fmt::Debug for MdxStft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MdxStft")
            .field("n_fft", &self.n_fft)
            .field("hop", &self.hop)
            .field("bins", &self.bins)
            .field("frames", &self.frames)
            .finish_non_exhaustive()
    }
}

/// Periodic Hann window of `length` samples, as `torch.hann_window`.
#[expect(
    clippy::cast_precision_loss,
    reason = "FFT sizes are a few thousand, exactly representable in f32"
)]
fn hann_window(length: usize) -> Vec<f32> {
    (0..length)
        .map(|n| {
            let phase = std::f32::consts::TAU * n as f32 / length as f32;
            0.5 - 0.5 * phase.cos()
        })
        .collect()
}

impl MdxStft {
    /// # Errors
    ///
    /// Returns an error if the parameters do not describe a usable window.
    pub(super) fn new(params: &MdxParams) -> Result<Self, StftError> {
        let (n_fft, hop, bins, frames) =
            (params.n_fft(), params.hop(), params.bins(), params.frames());
        let length = hop.saturating_mul(frames.saturating_sub(1));
        let pad = n_fft / 2;
        if hop == 0 || bins == 0 || bins > pad.saturating_add(1) || length <= pad {
            return Err(StftError::TooShort(length));
        }
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(n_fft);
        let inverse = planner.plan_fft_inverse(n_fft);
        let window = hann_window(n_fft);
        let padded_length = length.saturating_add(n_fft);

        let mut envelope = vec![0.0_f32; padded_length];
        for start in (0..frames).map(|frame| frame.saturating_mul(hop)) {
            let span =
                envelope
                    .get_mut(start..start.saturating_add(n_fft))
                    .ok_or(StftError::Range {
                        buffer: padded_length,
                        start,
                        end: start.saturating_add(n_fft),
                    })?;
            for (total, &weight) in span.iter_mut().zip(&window) {
                *total += weight * weight;
            }
        }

        Ok(Self {
            n_fft,
            hop,
            bins,
            frames,
            length,
            window,
            time: forward.make_input_vec(),
            spectrum: forward.make_output_vec(),
            forward_scratch: forward.make_scratch_vec(),
            inverse_scratch: inverse.make_scratch_vec(),
            forward,
            inverse,
            padded: Vec::with_capacity(padded_length),
            signal: vec![0.0; padded_length],
            envelope,
        })
    }

    /// Samples of one channel of a window.
    pub(super) const fn length(&self) -> usize {
        self.length
    }

    /// Values of one spectrogram plane.
    pub(super) const fn plane(&self) -> usize {
        self.bins * self.frames
    }

    fn check_plane(&self, plane: &[f32]) -> Result<(), StftError> {
        if plane.len() == self.plane() {
            Ok(())
        } else {
            Err(StftError::PlaneSize {
                expected: self.plane(),
                actual: plane.len(),
            })
        }
    }

    /// Writes the spectrogram of `channel`, one window long, into `real`
    /// and `imaginary`.
    pub(super) fn forward(
        &mut self,
        channel: &[f32],
        real: &mut [f32],
        imaginary: &mut [f32],
    ) -> Result<(), StftError> {
        self.check_plane(real)?;
        self.check_plane(imaginary)?;
        let pad = self.n_fft / 2;
        let length = channel.len();
        if length != self.length {
            return Err(StftError::TooShort(length));
        }
        // Reflection without repeating the edge sample, as `F.pad(mode="reflect")`.
        let head = channel.get(1..=pad).ok_or(StftError::TooShort(length))?;
        let tail = channel
            .get(length.saturating_sub(pad + 1)..length.saturating_sub(1))
            .ok_or(StftError::TooShort(length))?;
        self.padded.clear();
        self.padded.extend(head.iter().rev());
        self.padded.extend_from_slice(channel);
        self.padded.extend(tail.iter().rev());

        for frame in 0..self.frames {
            let start = frame.saturating_mul(self.hop);
            let samples = self
                .padded
                .get(start..start.saturating_add(self.n_fft))
                .ok_or(StftError::Range {
                    buffer: self.padded.len(),
                    start,
                    end: start.saturating_add(self.n_fft),
                })?;
            for ((target, &sample), &weight) in self.time.iter_mut().zip(samples).zip(&self.window)
            {
                *target = sample * weight;
            }
            self.forward.process_with_scratch(
                &mut self.time,
                &mut self.spectrum,
                &mut self.forward_scratch,
            )?;
            let rows = real
                .chunks_exact_mut(self.frames)
                .zip(imaginary.chunks_exact_mut(self.frames));
            for ((real_row, imaginary_row), value) in rows.zip(&self.spectrum) {
                if let (Some(re), Some(im)) =
                    (real_row.get_mut(frame), imaginary_row.get_mut(frame))
                {
                    *re = value.re;
                    *im = value.im;
                }
            }
        }
        Ok(())
    }

    /// Writes the signal whose spectrogram is `real` and `imaginary` into
    /// `channel`, one window long.
    pub(super) fn inverse(
        &mut self,
        real: &[f32],
        imaginary: &[f32],
        channel: &mut [f32],
    ) -> Result<(), StftError> {
        self.check_plane(real)?;
        self.check_plane(imaginary)?;
        if channel.len() != self.length {
            return Err(StftError::TooShort(channel.len()));
        }
        self.signal.fill(0.0);
        #[expect(
            clippy::cast_precision_loss,
            reason = "FFT sizes are a few thousand, exactly representable in f32"
        )]
        let scale = 1.0 / self.n_fft as f32;

        for frame in 0..self.frames {
            // Bins above the ones the network sees are silent.
            self.spectrum.fill(Complex32::new(0.0, 0.0));
            let rows = real
                .chunks_exact(self.frames)
                .zip(imaginary.chunks_exact(self.frames));
            for (slot, (real_row, imaginary_row)) in self.spectrum.iter_mut().zip(rows) {
                if let (Some(&re), Some(&im)) = (real_row.get(frame), imaginary_row.get(frame)) {
                    *slot = Complex32::new(re, im);
                }
            }
            // A real signal has real DC and Nyquist bins; the inverse refuses
            // anything else.
            if let Some(dc) = self.spectrum.first_mut() {
                dc.im = 0.0;
            }
            if let Some(nyquist) = self.spectrum.last_mut() {
                nyquist.im = 0.0;
            }
            self.inverse.process_with_scratch(
                &mut self.spectrum,
                &mut self.time,
                &mut self.inverse_scratch,
            )?;

            let start = frame.saturating_mul(self.hop);
            let buffer = self.signal.len();
            let span = self
                .signal
                .get_mut(start..start.saturating_add(self.n_fft))
                .ok_or(StftError::Range {
                    buffer,
                    start,
                    end: start.saturating_add(self.n_fft),
                })?;
            for ((total, &sample), &weight) in span.iter_mut().zip(&self.time).zip(&self.window) {
                *total += sample * weight * scale;
            }
        }

        let pad = self.n_fft / 2;
        let range = pad..pad.saturating_add(self.length);
        let signal = self.signal.get(range.clone()).ok_or(StftError::Range {
            buffer: self.signal.len(),
            start: range.start,
            end: range.end,
        })?;
        let envelope = self
            .envelope
            .get(range)
            .ok_or(StftError::TooShort(self.length))?;
        for ((sample, &value), &weight) in channel.iter_mut().zip(signal).zip(envelope) {
            *sample = value / weight.max(f32::EPSILON);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    /// Every bin kept: the round trip must return the signal.
    const FULL: MdxParams = MdxParams::new(64, 16, 33, 64, 1.0);

    #[expect(clippy::cast_precision_loss, reason = "test signal of small integers")]
    fn signal(length: usize) -> Vec<f32> {
        (0..length)
            .map(|index| {
                let time = index as f32;
                (time * 0.13).sin() * 0.6 + (time * 0.031).cos() * 0.3
            })
            .collect()
    }

    #[test]
    fn round_trip_with_every_bin_returns_the_signal() -> Result<(), Box<dyn Error>> {
        let mut stft = MdxStft::new(&FULL)?;
        let input = signal(stft.length());
        let mut real = vec![0.0; stft.plane()];
        let mut imaginary = vec![0.0; stft.plane()];
        let mut output = vec![0.0; stft.length()];

        stft.forward(&input, &mut real, &mut imaginary)?;
        stft.inverse(&real, &imaginary, &mut output)?;

        let worst = input
            .iter()
            .zip(&output)
            .fold(0.0_f32, |worst, (a, b)| worst.max((a - b).abs()));
        assert!(worst < 1e-5, "worst difference {worst}");
        Ok(())
    }

    #[test]
    fn spectrogram_has_the_frames_of_the_window() -> Result<(), Box<dyn Error>> {
        let params = MdxParams::new(6144, 1024, 3072, 512, 1.0);
        let stft = MdxStft::new(&params)?;

        assert_eq!(stft.length(), 523_264);
        assert_eq!(stft.plane(), 3072 * 512);
        Ok(())
    }

    #[test]
    fn dropped_bins_come_back_silent() -> Result<(), Box<dyn Error>> {
        // A tone in bin 24 of 32: above the 8 bins kept.
        let params = MdxParams::new(64, 16, 8, 64, 1.0);
        let mut stft = MdxStft::new(&params)?;
        #[expect(clippy::cast_precision_loss, reason = "test signal of small integers")]
        let input: Vec<f32> = (0..stft.length())
            .map(|index| (std::f32::consts::TAU * 24.0 * index as f32 / 64.0).sin())
            .collect();
        let mut real = vec![0.0; stft.plane()];
        let mut imaginary = vec![0.0; stft.plane()];
        let mut output = vec![0.0; stft.length()];

        stft.forward(&input, &mut real, &mut imaginary)?;
        stft.inverse(&real, &imaginary, &mut output)?;

        // Reflecting a tone at the edges bends it, which puts some of it in
        // every bin; away from the edges only the tone itself is there.
        let loudest = output
            .iter()
            .skip(64)
            .take(stft.length() - 128)
            .fold(0.0_f32, |loudest, &sample| loudest.max(sample.abs()));
        assert!(loudest < 1e-4, "loudest sample {loudest}");
        Ok(())
    }

    #[test]
    fn wrong_sizes_are_errors() -> Result<(), Box<dyn Error>> {
        let mut stft = MdxStft::new(&FULL)?;
        let mut real = vec![0.0; stft.plane()];
        let mut imaginary = vec![0.0; stft.plane() - 1];

        assert!(
            stft.forward(&signal(stft.length()), &mut real, &mut imaginary)
                .is_err()
        );
        let mut imaginary = vec![0.0; stft.plane()];
        assert!(
            stft.forward(&signal(10), &mut real, &mut imaginary)
                .is_err()
        );
        Ok(())
    }
}
