//! Checks whether the pinned `Reverb_HQ_By_FoxJoy` MDX-Net export is usable
//! through ONNX Runtime on this machine. The model predicts reverb, so the
//! cleaned signal is `mix - prediction * COMPENSATE`.

use std::{
    env, io,
    path::PathBuf,
    process::{Command, id},
    time::Instant,
};

use ort::{
    ep,
    session::Session,
    value::{TensorRef, ValueType},
};
use realfft::{RealFftPlanner, num_complex::Complex32};

const SAMPLE_RATE_HZ: f64 = 44_100.0;
const N_FFT: usize = 6_144;
const HOP: usize = 1_024;
const DIM_F: usize = 3_072;
const DIM_T: usize = 512;
const CHANNELS: usize = 2;
const WINDOW_FRAMES: usize = HOP * (DIM_T - 1);
const COMPENSATE: f32 = 1.035;
const DEFAULT_RUNS: usize = 3;
const N_FFT_F32: f32 = 6_144.0;

#[derive(Debug, thiserror::Error)]
enum ProbeError {
    #[error("usage: mdx-probe [model.onnx] [--runs N]")]
    Usage,
    #[error("invalid value `{value}` for {flag}")]
    InvalidArg { flag: &'static str, value: String },
    #[error("ONNX Runtime: {0}")]
    Ort(#[from] ort::Error),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("FFT: {0}")]
    Fft(#[from] realfft::FftError),
    #[error("model input or output does not match the expected MDX-Net contract")]
    Contract,
    #[error("model output `output` is missing")]
    MissingOutput,
    #[error("model output has {actual} samples, expected {expected}")]
    OutputSize { expected: usize, actual: usize },
    #[error("audio is too short for reflect padding")]
    TooShortForPadding,
}
struct Lcg(u32);

impl Lcg {
    fn sample(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let [_, _, low, high] = self.0.to_le_bytes();
        f64::from(u16::from_le_bytes([low, high])) / f64::from(u16::MAX) - 0.5
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the generated audio signal is deliberately bounded to the f32 sample range"
)]
fn narrow(sample: f64) -> f32 {
    sample as f32
}

#[expect(
    clippy::cast_precision_loss,
    reason = "the fixed inference window is exactly representable as f64"
)]
fn frames_as_seconds(frames: usize) -> f64 {
    frames as f64 / SAMPLE_RATE_HZ
}

#[derive(Debug)]
struct Args {
    model: PathBuf,
    runs: usize,
}

impl Args {
    fn parse(mut raw: impl Iterator<Item = String>) -> Result<Self, ProbeError> {
        let mut model = PathBuf::from("../../models/uvr/Reverb_HQ_By_FoxJoy.onnx");
        let mut runs = DEFAULT_RUNS;
        while let Some(value) = raw.next() {
            match value.as_str() {
                "--runs" => {
                    let count = raw.next().ok_or(ProbeError::Usage)?;
                    runs = count.parse().map_err(|_| ProbeError::InvalidArg {
                        flag: "--runs",
                        value: count,
                    })?;
                    if runs == 0 {
                        return Err(ProbeError::Usage);
                    }
                }
                value if value.starts_with("--") => return Err(ProbeError::Usage),
                value => model = PathBuf::from(value),
            }
        }
        Ok(Self { model, runs })
    }
}

#[derive(Debug, Clone, Copy)]
enum Device {
    Cpu,
    DirectMl,
}

#[derive(Debug)]
struct SessionRun {
    output: Vec<f32>,
    seconds: Vec<f64>,
}

#[derive(Debug, Clone, Copy)]
struct Memory {
    process_mib: Option<f64>,
    gpu_mib: Option<u32>,
}

fn build_session(model: &PathBuf, device: Device) -> ort::Result<Session> {
    let mut builder = Session::builder()?;
    match device {
        Device::Cpu => {}
        Device::DirectMl => {
            let direct_ml = ep::DirectML::default()
                .with_performance_preference(ep::directml::PerformancePreference::HighPerformance)
                .build()
                .error_on_failure();
            builder = builder
                .with_config_entry("ep.dml.disable_graph_fusion", "1")?
                .with_memory_pattern(false)?
                .with_parallel_execution(false)?
                .with_disable_cpu_fallback()?
                .with_execution_providers([direct_ml])?;
        }
    }
    builder.commit_from_file(model)
}

fn print_metadata(session: &Session) {
    for input in session.inputs() {
        println!("input: `{}` {:?}", input.name(), input.dtype());
    }
    for output in session.outputs() {
        println!("output: `{}` {:?}", output.name(), output.dtype());
    }
}

fn validate_contract(session: &Session) -> Result<(), ProbeError> {
    let input = session
        .inputs()
        .iter()
        .find(|metadata| metadata.name() == "input")
        .ok_or(ProbeError::Contract)?;
    let output = session
        .outputs()
        .iter()
        .find(|metadata| metadata.name() == "output")
        .ok_or(ProbeError::Contract)?;
    let expected = [-1_i64, 4, 3_072, 512];
    let valid = matches!(
        (input.dtype(), output.dtype()),
        (
            ValueType::Tensor {
                shape: input_shape,
                ..
            },
            ValueType::Tensor {
                shape: output_shape,
                ..
            }
        ) if input_shape.iter().copied().eq(expected)
            && output_shape.iter().copied().eq(expected)
    );
    valid.then_some(()).ok_or(ProbeError::Contract)
}

fn periodic_hann() -> Vec<f32> {
    (0..N_FFT)
        .map(|sample| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "the short window size is exactly representable as f32"
            )]
            let phase = std::f32::consts::TAU * sample as f32 / N_FFT as f32;
            0.5 - 0.5 * phase.cos()
        })
        .collect()
}

fn reflect_pad(signal: &[f32]) -> Result<Vec<f32>, ProbeError> {
    let padding = N_FFT / 2;
    if signal.len() <= padding {
        return Err(ProbeError::TooShortForPadding);
    }
    let mut padded = Vec::with_capacity(signal.len().saturating_add(N_FFT));
    padded.extend(signal.iter().skip(1).take(padding).rev());
    padded.extend_from_slice(signal);
    padded.extend(signal.iter().rev().skip(1).take(padding));
    Ok(padded)
}

/// Creates one `[DIM_F][DIM_T]` plane for each real and imaginary channel
/// component. Torch's `center=True` corresponds to the reflect padding here.
fn stft(channel: &[f32]) -> Result<(Vec<f32>, Vec<f32>), ProbeError> {
    let padded = reflect_pad(channel)?;
    let window = periodic_hann();
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
    let mut input = fft.make_input_vec();
    let mut output = fft.make_output_vec();
    let mut frames = Vec::with_capacity(DIM_T);

    for frame in padded.windows(N_FFT).step_by(HOP) {
        for ((target, &sample), &weight) in input.iter_mut().zip(frame).zip(&window) {
            *target = sample * weight;
        }
        fft.process(&mut input, &mut output)?;
        frames.push(output.clone());
    }
    if frames.len() != DIM_T {
        return Err(ProbeError::Contract);
    }

    let capacity = DIM_F * DIM_T;
    let mut real = Vec::with_capacity(capacity);
    let mut imaginary = Vec::with_capacity(capacity);
    for bin in 0..DIM_F {
        real.extend(
            frames
                .iter()
                .filter_map(|frame| frame.get(bin))
                .map(|value| value.re),
        );
        imaginary.extend(
            frames
                .iter()
                .filter_map(|frame| frame.get(bin))
                .map(|value| value.im),
        );
    }
    Ok((real, imaginary))
}

fn model_input(stereo: &[[f32; CHANNELS]]) -> Result<Vec<f32>, ProbeError> {
    let left: Vec<f32> = stereo.iter().map(|&[left, _]| left).collect();
    let right: Vec<f32> = stereo.iter().map(|&[_, right]| right).collect();
    let (left_real, left_imaginary) = stft(&left)?;
    let (right_real, right_imaginary) = stft(&right)?;
    Ok([left_real, left_imaginary, right_real, right_imaginary].concat())
}

/// Restores a centered waveform after the model's dropped Nyquist bin is put
/// back as zero. The FFT crate's inverse is unnormalised.
fn inverse(real: &[f32], imaginary: &[f32]) -> Result<Vec<f32>, ProbeError> {
    let expected = DIM_F * DIM_T;
    if real.len() != expected || imaginary.len() != expected {
        return Err(ProbeError::OutputSize {
            expected,
            actual: real.len().min(imaginary.len()),
        });
    }
    let window = periodic_hann();
    let fft = RealFftPlanner::<f32>::new().plan_fft_inverse(N_FFT);
    let mut spectrum = fft.make_input_vec();
    let mut time = fft.make_output_vec();
    let padded_len = WINDOW_FRAMES.saturating_add(N_FFT);
    let mut signal = vec![0.0_f32; padded_len];
    let mut envelope = vec![0.0_f32; padded_len];

    for (frame, start) in (0..DIM_T).zip((0_usize..).step_by(HOP)) {
        for ((slot, re), im) in spectrum
            .iter_mut()
            .zip(real.chunks_exact(DIM_T))
            .zip(imaginary.chunks_exact(DIM_T))
        {
            *slot = Complex32::new(
                re.get(frame).copied().ok_or(ProbeError::Contract)?,
                im.get(frame).copied().ok_or(ProbeError::Contract)?,
            );
        }
        let nyquist = spectrum.last_mut().ok_or(ProbeError::Contract)?;
        *nyquist = Complex32::new(0.0, 0.0);
        let dc = spectrum.first_mut().ok_or(ProbeError::Contract)?;
        dc.im = 0.0;
        fft.process(&mut spectrum, &mut time)?;

        let end = start.saturating_add(N_FFT);
        let target = signal.get_mut(start..end).ok_or(ProbeError::Contract)?;
        let weights = envelope.get_mut(start..end).ok_or(ProbeError::Contract)?;
        for (((sample, weight), &restored), &window_weight) in target
            .iter_mut()
            .zip(weights.iter_mut())
            .zip(&time)
            .zip(&window)
        {
            *sample += restored * window_weight / N_FFT_F32;
            *weight += window_weight * window_weight;
        }
    }

    let padding = N_FFT / 2;
    Ok(signal
        .get(padding..padding.saturating_add(WINDOW_FRAMES))
        .ok_or(ProbeError::Contract)?
        .iter()
        .zip(
            envelope
                .get(padding..padding.saturating_add(WINDOW_FRAMES))
                .ok_or(ProbeError::Contract)?,
        )
        .map(|(&sample, &weight)| sample / weight.max(f32::EPSILON))
        .collect())
}

fn restore_stereo(output: &[f32]) -> Result<Vec<[f32; CHANNELS]>, ProbeError> {
    let plane = DIM_F * DIM_T;
    let planes: Vec<&[f32]> = output.chunks_exact(plane).collect();
    let [left_real, left_imaginary, right_real, right_imaginary] = planes.as_slice() else {
        return Err(ProbeError::OutputSize {
            expected: plane * 4,
            actual: output.len(),
        });
    };
    let left = inverse(left_real, left_imaginary)?;
    let right = inverse(right_real, right_imaginary)?;
    Ok(left
        .iter()
        .zip(&right)
        .map(|(&left, &right)| [left, right])
        .collect())
}

fn run_session(
    session: &mut Session,
    input: &[f32],
    runs: usize,
) -> Result<SessionRun, ProbeError> {
    let shape = vec![1_i64, 4, 3_072, 512];
    let mut output = Vec::new();
    let mut seconds = Vec::with_capacity(runs);
    for run in 1..=runs {
        let tensor = TensorRef::from_array_view((shape.clone(), input))?;
        let start = Instant::now();
        let outputs = session.run(vec![("input", tensor)])?;
        let elapsed = start.elapsed().as_secs_f64();
        let value = outputs.get("output").ok_or(ProbeError::MissingOutput)?;
        let (_, data) = value.try_extract_tensor::<f32>()?;
        if data.len() != input.len() {
            return Err(ProbeError::OutputSize {
                expected: input.len(),
                actual: data.len(),
            });
        }
        println!("  run {run}: {elapsed:.3} s");
        seconds.push(elapsed);
        output = data.to_vec();
    }
    Ok(SessionRun { output, seconds })
}

fn median(seconds: &mut [f64]) -> Result<f64, ProbeError> {
    seconds.sort_by(f64::total_cmp);
    seconds
        .get(seconds.len() / 2)
        .copied()
        .ok_or(ProbeError::Contract)
}

fn rms(samples: impl Iterator<Item = f32>) -> f64 {
    let (sum, count) = samples.fold((0.0_f64, 0_usize), |(sum, count), value| {
        (sum + f64::from(value).powi(2), count.saturating_add(1))
    });
    #[expect(
        clippy::cast_precision_loss,
        reason = "one MDX inference window has far fewer samples than f64 can count exactly"
    )]
    let count = count as f64;
    (sum / count.max(1.0)).sqrt()
}

fn difference(cpu: &[f32], direct_ml: &[f32]) -> Result<(), ProbeError> {
    if cpu.len() != direct_ml.len() {
        return Err(ProbeError::OutputSize {
            expected: cpu.len(),
            actual: direct_ml.len(),
        });
    }
    let max = cpu
        .iter()
        .zip(direct_ml)
        .map(|(&cpu, &direct_ml)| (cpu - direct_ml).abs())
        .fold(0.0_f32, f32::max);
    let rms = rms(cpu
        .iter()
        .zip(direct_ml)
        .map(|(&cpu, &direct_ml)| cpu - direct_ml));
    println!("CPU vs DirectML raw output: max |diff| {max:.7}, RMS diff {rms:.7}");
    Ok(())
}

fn process_memory_mib() -> Option<f64> {
    let process_id = id();
    let command = format!(
        "([double]((Get-Process -Id {process_id}).WorkingSet64 / 1MB)).ToString([System.Globalization.CultureInfo]::InvariantCulture)"
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", &command])
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

fn gpu_memory_mib() -> Option<u32> {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=memory.used", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

fn memory() -> Memory {
    Memory {
        process_mib: process_memory_mib(),
        gpu_mib: gpu_memory_mib(),
    }
}

fn synth_mix() -> Vec<[f32; CHANNELS]> {
    let mut phase_left = 0.0_f64;
    let mut phase_right = 0.3_f64;
    let mut noise = Lcg(0x1234_5678);
    let left_step = std::f64::consts::TAU * 220.0 / SAMPLE_RATE_HZ;
    let right_step = std::f64::consts::TAU * 330.0 / SAMPLE_RATE_HZ;
    (0..WINDOW_FRAMES)
        .map(|_| {
            let noise = noise.sample();
            let left = narrow(phase_left.sin() * 0.45 + noise * 0.05);
            let right = narrow(phase_right.sin() * 0.45 + noise * 0.05);
            phase_left += left_step;
            phase_right += right_step;
            [left, right]
        })
        .collect()
}

fn tail_mix() -> Vec<[f32; CHANNELS]> {
    const DRY_FRAMES: usize = 88_200;
    let mut phase = 0.0_f64;
    let step = std::f64::consts::TAU * 440.0 / SAMPLE_RATE_HZ;
    (0..WINDOW_FRAMES)
        .map(|frame| {
            let tail_frame = frame.saturating_sub(DRY_FRAMES);
            #[expect(
                clippy::cast_precision_loss,
                reason = "the sample number is small relative to f64 precision"
            )]
            let tail_seconds = tail_frame as f64 / SAMPLE_RATE_HZ;
            let envelope = if frame < DRY_FRAMES {
                0.7
            } else {
                0.7 * (-tail_seconds / 1.5).exp()
            };
            let left = narrow(envelope * phase.sin());
            let right = narrow(envelope * (phase + 0.7).sin());
            phase += step;
            [left, right]
        })
        .collect()
}

fn cleanup(mix: &[[f32; CHANNELS]], prediction: &[[f32; CHANNELS]]) -> Vec<[f32; CHANNELS]> {
    mix.iter()
        .zip(prediction)
        .map(|(&[mix_left, mix_right], &[out_left, out_right])| {
            [
                mix_left - out_left * COMPENSATE,
                mix_right - out_right * COMPENSATE,
            ]
        })
        .collect()
}

fn channel_samples(stereo: &[[f32; CHANNELS]]) -> impl Iterator<Item = f32> + '_ {
    stereo.iter().flat_map(|&[left, right]| [left, right])
}

fn report_sanity(
    session: &mut Session,
    silence: &[[f32; CHANNELS]],
    tail: &[[f32; CHANNELS]],
) -> Result<(), ProbeError> {
    let silence_output = run_session(session, &model_input(silence)?, 1)?;
    let silence_prediction = restore_stereo(&silence_output.output)?;
    println!(
        "silence prediction RMS: {:.9}",
        rms(channel_samples(&silence_prediction))
    );

    let tail_output = run_session(session, &model_input(tail)?, 1)?;
    let tail_prediction = restore_stereo(&tail_output.output)?;
    let cleaned = cleanup(tail, &tail_prediction);
    let tail_start = 88_200_usize;
    let tail_length = 132_300_usize;
    let input_energy = rms(channel_samples(
        tail.get(tail_start..tail_start.saturating_add(tail_length))
            .ok_or(ProbeError::Contract)?,
    ));
    let clean_energy = rms(channel_samples(
        cleaned
            .get(tail_start..tail_start.saturating_add(tail_length))
            .ok_or(ProbeError::Contract)?,
    ));
    println!(
        "artificial tail RMS: input {input_energy:.6}, cleaned {clean_energy:.6}, ratio {:.3}",
        clean_energy / input_energy.max(f64::EPSILON)
    );
    Ok(())
}

fn main() -> Result<(), ProbeError> {
    let args = Args::parse(env::args().skip(1))?;
    let metadata = std::fs::metadata(&args.model).map_err(|source| ProbeError::Io {
        path: args.model.clone(),
        source,
    })?;
    println!(
        "model: {} ({} bytes); window: {} frames ({:.3} s)",
        args.model.display(),
        metadata.len(),
        WINDOW_FRAMES,
        frames_as_seconds(WINDOW_FRAMES)
    );

    let input_mix = synth_mix();
    let input = model_input(&input_mix)?;

    let cpu_start = Instant::now();
    let mut cpu = build_session(&args.model, Device::Cpu)?;
    println!(
        "CPU session created in {:.3} s",
        cpu_start.elapsed().as_secs_f64()
    );
    print_metadata(&cpu);
    validate_contract(&cpu)?;
    let mut cpu_run = run_session(&mut cpu, &input, args.runs)?;
    println!("CPU median run: {:.3} s", median(&mut cpu_run.seconds)?);

    let before_direct_ml = memory();
    let direct_ml_start = Instant::now();
    let mut direct_ml = build_session(&args.model, Device::DirectMl)?;
    let after_direct_ml_load = memory();
    println!(
        "DirectML session created in {:.3} s",
        direct_ml_start.elapsed().as_secs_f64()
    );
    print_metadata(&direct_ml);
    validate_contract(&direct_ml)?;
    let mut direct_ml_run = run_session(&mut direct_ml, &input, args.runs)?;
    let after_direct_ml_runs = memory();
    println!(
        "DirectML median run: {:.3} s",
        median(&mut direct_ml_run.seconds)?
    );
    println!(
        "memory MiB: before DML process {:?}, GPU {:?}; loaded process {:?}, GPU {:?}; after runs process {:?}, GPU {:?}",
        before_direct_ml.process_mib,
        before_direct_ml.gpu_mib,
        after_direct_ml_load.process_mib,
        after_direct_ml_load.gpu_mib,
        after_direct_ml_runs.process_mib,
        after_direct_ml_runs.gpu_mib
    );
    difference(&cpu_run.output, &direct_ml_run.output)?;

    let silence = vec![[0.0_f32; CHANNELS]; WINDOW_FRAMES];
    report_sanity(&mut direct_ml, &silence, &tail_mix())?;
    Ok(())
}
