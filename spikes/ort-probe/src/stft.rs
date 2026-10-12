//! Demucs-compatible STFT and inverse STFT: the host-side counterpart of
//! `_spec` and `_ispec` in `demucs/htdemucs.py` for exports that leave the
//! complex transforms out of the graph.

use realfft::{RealFftPlanner, num_complex::Complex32};

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
pub(crate) enum StftError {
    #[error("signal of {0} samples is too short to reflect-pad")]
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

/// Spectrogram of one channel: `ceil(len / HOP)` frames of `BINS` bins each.
pub(crate) fn spectrogram(channel: &[f32]) -> Result<Vec<Vec<Complex32>>, StftError> {
    let frames = channel.len().div_ceil(HOP);
    let right = (PAD + frames * HOP).saturating_sub(channel.len());
    let padded = reflect_pad(channel, PAD, right)?;
    let centered = reflect_pad(&padded, N_FFT / 2, N_FFT / 2)?;

    let window = hann_window();
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
    let mut input = fft.make_input_vec();
    let mut output = fft.make_output_vec();
    let mut spectra = Vec::with_capacity(frames);
    for frame in centered
        .windows(N_FFT)
        .step_by(HOP)
        .skip(EDGE_FRAMES)
        .take(frames)
    {
        for ((slot, &sample), &weight) in input.iter_mut().zip(frame).zip(&window) {
            *slot = sample * weight;
        }
        fft.process(&mut input, &mut output)?;
        spectra.push(output.iter().take(BINS).map(|bin| bin * SCALE).collect());
    }
    Ok(spectra)
}

/// Packs stereo spectra as "complex as channels": planes `[left re, left im,
/// right re, right im]`, each laid out as `[BINS][frames]`.
pub(crate) fn pack_complex_as_channels(
    left: &[Vec<Complex32>],
    right: &[Vec<Complex32>],
) -> Vec<f32> {
    type Part = fn(&Complex32) -> f32;
    let planes: [(&[Vec<Complex32>], Part); 4] = [
        (left, |bin| bin.re),
        (left, |bin| bin.im),
        (right, |bin| bin.re),
        (right, |bin| bin.im),
    ];
    let mut packed = Vec::with_capacity(4 * BINS * left.len());
    for (spectra, part) in planes {
        for bin in 0..BINS {
            packed.extend(spectra.iter().filter_map(|frame| frame.get(bin)).map(part));
        }
    }
    packed
}

/// Inverse STFT of one channel given its real and imaginary planes, each laid
/// out as `[BINS][frames]`. Returns exactly `length` samples.
pub(crate) fn inverse(
    real: &[f32],
    imaginary: &[f32],
    frames: usize,
    length: usize,
) -> Result<Vec<f32>, StftError> {
    let expected = BINS * frames;
    for plane in [real, imaginary] {
        if frames == 0 || plane.len() != expected {
            return Err(StftError::PlaneSize {
                expected,
                actual: plane.len(),
            });
        }
    }

    let window = hann_window();
    let fft = RealFftPlanner::<f32>::new().plan_fft_inverse(N_FFT);
    let mut spectrum = fft.make_input_vec();
    let mut time = fft.make_output_vec();

    let total_frames = frames + 2 * EDGE_FRAMES;
    let buffer_len = N_FFT + HOP * (total_frames - 1);
    let mut signal = vec![0.0_f32; buffer_len];
    let mut envelope = vec![0.0_f32; buffer_len];
    let out_of_range = |start: usize, end: usize| StftError::Range {
        buffer: buffer_len,
        start,
        end,
    };

    // The zero frames torch pads on both sides still contribute to the envelope.
    for start in (0..total_frames).map(|frame| frame * HOP) {
        let end = start + N_FFT;
        let target = envelope
            .get_mut(start..end)
            .ok_or_else(|| out_of_range(start, end))?;
        for (acc, &weight) in target.iter_mut().zip(&window) {
            *acc += weight * weight;
        }
    }

    for frame in 0..frames {
        let bins = real
            .chunks_exact(frames)
            .zip(imaginary.chunks_exact(frames))
            .map(|(re, im)| (re.get(frame).copied(), im.get(frame).copied()));
        // `zip` stops after `BINS` rows, so the Nyquist slot is cleared separately.
        for (slot, (re, im)) in spectrum.iter_mut().zip(bins) {
            *slot = Complex32::new(re.unwrap_or(0.0), im.unwrap_or(0.0));
        }
        if let Some(nyquist) = spectrum.last_mut() {
            *nyquist = Complex32::new(0.0, 0.0);
        }
        // A real signal has a real DC bin; the inverse transform rejects anything else.
        if let Some(dc) = spectrum.first_mut() {
            dc.im = 0.0;
        }
        fft.process(&mut spectrum, &mut time)?;

        let start = (frame + EDGE_FRAMES) * HOP;
        let end = start + N_FFT;
        let target = signal
            .get_mut(start..end)
            .ok_or_else(|| out_of_range(start, end))?;
        for ((acc, &sample), &weight) in target.iter_mut().zip(&time).zip(&window) {
            *acc += sample * SCALE * weight;
        }
    }

    // Undo torch's centering (N_FFT / 2) and Demucs' own padding (PAD).
    let start = N_FFT / 2 + PAD;
    let end = start + length;
    let samples = signal
        .get(start..end)
        .ok_or_else(|| out_of_range(start, end))?;
    let weights = envelope
        .get(start..end)
        .ok_or_else(|| out_of_range(start, end))?;
    Ok(samples
        .iter()
        .zip(weights)
        .map(|(&sample, &weight)| sample / weight.max(1e-11))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflect_pad_mirrors_without_repeating_the_edge() -> Result<(), StftError> {
        let padded = reflect_pad(&[1.0, 2.0, 3.0, 4.0], 2, 1)?;
        assert_eq!(padded, [3.0, 2.0, 1.0, 2.0, 3.0, 4.0, 3.0]);
        Ok(())
    }

    #[test]
    fn inverse_restores_the_signal() -> Result<(), StftError> {
        let length = 5 * HOP + 300;
        let signal: Vec<f32> = (0..length)
            .scan(0.0_f32, |phase, _| {
                *phase += 0.05;
                Some(phase.sin() + 0.3 * (*phase * 7.1).cos())
            })
            .collect();

        let spectra = spectrogram(&signal)?;
        let frames = spectra.len();
        assert_eq!(frames, 6);
        let packed = pack_complex_as_channels(&spectra, &spectra);
        let plane = BINS * frames;
        let (Some(real), Some(imaginary)) = (packed.get(..plane), packed.get(plane..2 * plane))
        else {
            return Err(StftError::PlaneSize {
                expected: 2 * plane,
                actual: packed.len(),
            });
        };
        let restored = inverse(real, imaginary, frames, length)?;

        assert_eq!(restored.len(), length);
        // Demucs keeps `frames` of the `frames + 4` STFT frames and restores the
        // rest as silence, so the first and last `PAD` samples come back
        // attenuated by design. Everything in between must survive, the only
        // lossy step there being the dropped Nyquist bin.
        let interior = PAD..frames * HOP - PAD;
        let (Some(expected), Some(actual)) = (signal.get(interior.clone()), restored.get(interior))
        else {
            return Err(StftError::TooShort(length));
        };
        let worst = expected
            .iter()
            .zip(actual)
            .fold(0.0_f32, |acc, (a, b)| acc.max((a - b).abs()));
        assert!(worst < 1e-3, "max interior error {worst}");
        Ok(())
    }
}
