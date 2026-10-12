//! Deterministic test input and the statistics the probe reports.

pub(crate) const SAMPLE_RATE_HZ: f32 = 44_100.0;

/// Deterministic noise source, so every run and device sees the same input.
pub(crate) struct Lcg(pub(crate) u32);

impl Lcg {
    /// Next value in `[-1, 1)`.
    pub(crate) fn next_sample(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let [_, _, low, high] = self.0.to_le_bytes();
        f32::from(u16::from_le_bytes([low, high])) / 32_768.0 - 1.0
    }
}

/// Phase accumulator; avoids the precision loss of a growing time value.
struct Oscillator {
    phase: f32,
    step: f32,
}

impl Oscillator {
    fn new(frequency_hz: f32, phase: f32) -> Self {
        Self {
            phase,
            step: std::f32::consts::TAU * frequency_hz / SAMPLE_RATE_HZ,
        }
    }

    fn next_sample(&mut self) -> f32 {
        let value = self.phase.sin();
        self.phase += self.step;
        if self.phase >= std::f32::consts::TAU {
            self.phase -= std::f32::consts::TAU;
        }
        value
    }
}

/// Planar stereo test signal (left channel, then right): a 55 Hz tone, a
/// chord and decaying noise bursts twice a second. Not music, but it has the
/// rough spectral roles of bass, harmony and percussion.
pub(crate) fn synth_mix(frames: usize) -> Vec<f32> {
    const HIT_PERIOD_FRAMES: usize = 22_050;
    const HIT_DECAY: f32 = 0.999_25;

    let mut bass = Oscillator::new(55.0, 0.0);
    let mut chord_left = [
        Oscillator::new(440.0, 0.0),
        Oscillator::new(554.37, 0.0),
        Oscillator::new(659.25, 0.0),
    ];
    let mut chord_right = [
        Oscillator::new(440.0, 0.7),
        Oscillator::new(554.37, 1.9),
        Oscillator::new(659.25, 0.3),
    ];
    let mut noise = Lcg(0x1234_5678);
    let mut envelope = 0.0_f32;

    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for frame in 0..frames {
        if frame.is_multiple_of(HIT_PERIOD_FRAMES) {
            envelope = 1.0;
        }
        let low = 0.4 * bass.next_sample();
        let hit = 0.5 * envelope * noise.next_sample();
        envelope *= HIT_DECAY;
        let harmony_left: f32 = chord_left.iter_mut().map(Oscillator::next_sample).sum();
        let harmony_right: f32 = chord_right.iter_mut().map(Oscillator::next_sample).sum();
        left.push(low + hit + 0.1 * harmony_left);
        right.push(low + hit + 0.1 * harmony_right);
    }
    left.extend(right);
    left
}

#[expect(
    clippy::cast_precision_loss,
    reason = "sample counts here are far below 2^52"
)]
pub(crate) fn count(len: usize) -> f64 {
    len as f64
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "normalized audio samples are within f32 range"
)]
fn narrow(value: f64) -> f32 {
    value as f32
}

pub(crate) fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let energy: f64 = samples.iter().map(|&s| f64::from(s).powi(2)).sum();
    (energy / count(samples.len())).sqrt()
}

/// Brings the mix to zero mean and unit deviation, as Demucs does before
/// inference. Demucs takes the statistics from the mono downmix; the probe uses
/// the whole planar buffer, which only changes the overall scale and is the
/// same for every export and device being compared.
pub(crate) fn normalize(mix: &mut [f32]) {
    if mix.is_empty() {
        return;
    }
    let mean = mix.iter().map(|&s| f64::from(s)).sum::<f64>() / count(mix.len());
    let variance = mix
        .iter()
        .map(|&s| (f64::from(s) - mean).powi(2))
        .sum::<f64>()
        / count(mix.len());
    let deviation = variance.sqrt().max(1e-8);
    for sample in mix.iter_mut() {
        *sample = narrow((f64::from(*sample) - mean) / deviation);
    }
}
