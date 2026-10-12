//! Stage 0 probe: runs an `HTDemucs` ONNX export through ONNX Runtime on one
//! synthetic 7.8 s chunk and reports load time, per-run time, GPU memory and
//! output sanity. Final stems can be dumped and compared across devices and
//! across exports.

mod signal;
mod stft;

use std::{
    env, fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use ort::{
    ep,
    session::Session,
    value::{TensorRef, ValueType},
};

use crate::signal::{SAMPLE_RATE_HZ, count, normalize, rms, synth_mix};

/// `HTDemucs` training segment: 7.8 s at 44.1 kHz.
const SEGMENT_FRAMES: usize = 343_980;
const CHANNELS: usize = 2;

#[derive(Debug, thiserror::Error)]
enum ProbeError {
    #[error(
        "usage: ort-probe <model.onnx> [--device cpu|dml] [--runs N] [--threads N] \
         [--no-cpu-fallback] [--ort-defaults] [--fix-dim NAME=SIZE] [--config KEY=VALUE] \
         [--dump FILE] [--compare FILE]"
    )]
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
    #[error(transparent)]
    Stft(#[from] stft::StftError),
    #[error("model inputs match neither the waveform nor the hybrid contract")]
    UnsupportedContract,
    #[error("size {0} does not fit a tensor dimension")]
    Dimension(usize),
    #[error("output `{0}` is missing from the run result")]
    MissingOutput(String),
    #[error("model produced no output of rank {0}")]
    MissingRank(usize),
    #[error("unexpected output shape {0:?}")]
    OutputShape(Vec<i64>),
    #[error("reference has {reference} samples, this run produced {output}")]
    CompareLength { reference: usize, output: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Device {
    Cpu,
    DirectMl,
}

/// Whether to apply the session options ONNX Runtime documents for `DirectML`
/// (no memory pattern, sequential execution) or leave everything at defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionTuning {
    Documented,
    OrtDefaults,
}

/// Whether nodes the execution provider cannot run may fall back to the CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CpuFallback {
    Allowed,
    Forbidden,
}

#[derive(Debug)]
struct Args {
    model: PathBuf,
    device: Device,
    runs: NonZeroUsize,
    threads: Option<NonZeroUsize>,
    tuning: SessionTuning,
    fallback: CpuFallback,
    dump: Option<PathBuf>,
    compare: Option<PathBuf>,
    /// Symbolic dimensions pinned to a value, so the provider sees static shapes.
    fixed_dims: Vec<(String, i64)>,
    /// Raw ONNX Runtime session config entries.
    config: Vec<(String, String)>,
}

impl Args {
    fn parse(mut raw: impl Iterator<Item = String>) -> Result<Self, ProbeError> {
        let model = raw.next().map(PathBuf::from).ok_or(ProbeError::Usage)?;
        let mut args = Self {
            model,
            device: Device::Cpu,
            runs: NonZeroUsize::MIN,
            threads: None,
            tuning: SessionTuning::Documented,
            fallback: CpuFallback::Allowed,
            dump: None,
            compare: None,
            fixed_dims: Vec::new(),
            config: Vec::new(),
        };
        while let Some(flag) = raw.next() {
            match flag.as_str() {
                "--device" => {
                    let value = raw.next().ok_or(ProbeError::Usage)?;
                    args.device = match value.as_str() {
                        "cpu" => Device::Cpu,
                        "dml" => Device::DirectMl,
                        _ => {
                            return Err(ProbeError::InvalidArg {
                                flag: "--device",
                                value,
                            });
                        }
                    };
                }
                "--runs" => args.runs = parse_count("--runs", raw.next())?,
                "--threads" => args.threads = Some(parse_count("--threads", raw.next())?),
                "--no-cpu-fallback" => args.fallback = CpuFallback::Forbidden,
                "--ort-defaults" => args.tuning = SessionTuning::OrtDefaults,
                "--dump" => args.dump = Some(parse_path(raw.next())?),
                "--compare" => args.compare = Some(parse_path(raw.next())?),
                "--fix-dim" => args.fixed_dims.push(parse_dim(raw.next())?),
                "--config" => args.config.push(parse_config(raw.next())?),
                _ => return Err(ProbeError::Usage),
            }
        }
        Ok(args)
    }
}

fn parse_path(value: Option<String>) -> Result<PathBuf, ProbeError> {
    value.map(PathBuf::from).ok_or(ProbeError::Usage)
}

fn parse_count(flag: &'static str, value: Option<String>) -> Result<NonZeroUsize, ProbeError> {
    let value = value.ok_or(ProbeError::Usage)?;
    value
        .parse()
        .map_err(|_| ProbeError::InvalidArg { flag, value })
}

/// Parses `name=size`.
fn parse_dim(value: Option<String>) -> Result<(String, i64), ProbeError> {
    let value = value.ok_or(ProbeError::Usage)?;
    let parsed = value
        .split_once('=')
        .and_then(|(name, size)| Some((name.to_owned(), size.parse().ok()?)));
    parsed.ok_or(ProbeError::InvalidArg {
        flag: "--fix-dim",
        value,
    })
}

/// Parses `key=value`.
fn parse_config(value: Option<String>) -> Result<(String, String), ProbeError> {
    let value = value.ok_or(ProbeError::Usage)?;
    let parsed = value
        .split_once('=')
        .map(|(key, entry)| (key.to_owned(), entry.to_owned()));
    parsed.ok_or(ProbeError::InvalidArg {
        flag: "--config",
        value,
    })
}

fn build_session(args: &Args) -> ort::Result<Session> {
    let mut builder = Session::builder()?;
    for (key, entry) in &args.config {
        builder = builder.with_config_entry(key, entry)?;
    }
    for (name, size) in &args.fixed_dims {
        builder = builder.with_dimension_override(name, *size)?;
    }
    if let Some(threads) = args.threads {
        builder = builder.with_intra_threads(threads.get())?;
    }
    match args.device {
        Device::Cpu => {}
        Device::DirectMl => {
            let directml = ep::DirectML::default()
                .with_performance_preference(ep::directml::PerformancePreference::HighPerformance)
                .build()
                .error_on_failure();
            builder = builder.with_execution_providers([directml])?;
            match args.tuning {
                SessionTuning::Documented => {
                    builder = builder
                        .with_memory_pattern(false)?
                        .with_parallel_execution(false)?;
                }
                SessionTuning::OrtDefaults => {}
            }
        }
    }
    match args.fallback {
        CpuFallback::Allowed => {}
        CpuFallback::Forbidden => builder = builder.with_disable_cpu_fallback()?,
    }
    builder.commit_from_file(&args.model)
}

/// How an export divides the work between the graph and the host.
#[derive(Debug)]
enum Contract {
    /// Waveform in, stems out: STFT and inverse STFT are inside the graph.
    Waveform { mix: String },
    /// The host computes the STFT, feeds it next to the waveform, and adds the
    /// inverse STFT of the spectral output to the time-domain output.
    Hybrid { mix: String, spectrogram: String },
}

fn tensor_rank(dtype: &ValueType) -> Option<usize> {
    match dtype {
        ValueType::Tensor { shape, .. } => Some(shape.len()),
        ValueType::Sequence(_) | ValueType::Map { .. } | ValueType::Optional(_) => None,
    }
}

impl Contract {
    fn detect(session: &Session) -> Result<Self, ProbeError> {
        let ranked: Vec<(Option<usize>, &str)> = session
            .inputs()
            .iter()
            .map(|input| (tensor_rank(input.dtype()), input.name()))
            .collect();
        match *ranked.as_slice() {
            [(Some(3), mix)] => Ok(Self::Waveform {
                mix: mix.to_owned(),
            }),
            [(Some(3), mix), (Some(4), spectrogram)] | [(Some(4), spectrogram), (Some(3), mix)] => {
                Ok(Self::Hybrid {
                    mix: mix.to_owned(),
                    spectrogram: spectrogram.to_owned(),
                })
            }
            _ => Err(ProbeError::UnsupportedContract),
        }
    }
}

#[derive(Debug)]
struct InputTensor {
    name: String,
    shape: Vec<i64>,
    data: Vec<f32>,
}

#[derive(Debug)]
struct OutputTensor {
    name: String,
    shape: Vec<i64>,
    data: Vec<f32>,
}

fn dimension(size: usize) -> Result<i64, ProbeError> {
    i64::try_from(size).map_err(|_| ProbeError::Dimension(size))
}

/// Builds the model inputs for a planar stereo mix (left channel, then right).
fn build_inputs(contract: &Contract, mix: &[f32]) -> Result<Vec<InputTensor>, ProbeError> {
    let frames = mix.len() / CHANNELS;
    let waveform = |name: &str| -> Result<InputTensor, ProbeError> {
        Ok(InputTensor {
            name: name.to_owned(),
            shape: vec![1, 2, dimension(frames)?],
            data: mix.to_vec(),
        })
    };
    match contract {
        Contract::Waveform { mix: name } => Ok(vec![waveform(name)?]),
        Contract::Hybrid {
            mix: name,
            spectrogram,
        } => {
            let (left, right) = mix
                .split_at_checked(frames)
                .ok_or(ProbeError::Dimension(frames))?;
            let start = Instant::now();
            let left = stft::spectrogram(left)?;
            let right = stft::spectrogram(right)?;
            let data = stft::pack_complex_as_channels(&left, &right);
            println!(
                "host STFT: {:.1} ms",
                start.elapsed().as_secs_f64() * 1000.0
            );
            Ok(vec![
                waveform(name)?,
                InputTensor {
                    name: spectrogram.clone(),
                    shape: vec![1, 4, dimension(stft::BINS)?, dimension(left.len())?],
                    data,
                },
            ])
        }
    }
}

fn output_of_rank(outputs: &[OutputTensor], rank: usize) -> Result<&OutputTensor, ProbeError> {
    outputs
        .iter()
        .find(|output| output.shape.len() == rank)
        .ok_or(ProbeError::MissingRank(rank))
}

/// Turns raw model outputs into stems laid out as `[stems][2][frames]`.
fn final_stems(
    contract: &Contract,
    outputs: &[OutputTensor],
    frames: usize,
) -> Result<Vec<f32>, ProbeError> {
    let time_branch = output_of_rank(outputs, 4)?;
    match contract {
        Contract::Waveform { .. } => Ok(time_branch.data.clone()),
        Contract::Hybrid { .. } => {
            let spectral = output_of_rank(outputs, 5)?;
            let bad_shape = || ProbeError::OutputShape(spectral.shape.clone());
            let [1, _, 4, _, spectral_frames] = *spectral.shape.as_slice() else {
                return Err(bad_shape());
            };
            let spectral_frames = usize::try_from(spectral_frames).map_err(|_| bad_shape())?;
            let plane = stft::BINS * spectral_frames;
            if plane == 0 || frames == 0 {
                return Err(bad_shape());
            }

            let start = Instant::now();
            let planes: Vec<&[f32]> = spectral.data.chunks_exact(plane).collect();
            let mut stems = Vec::with_capacity(time_branch.data.len());
            for (pair, time) in planes
                .chunks_exact(2)
                .zip(time_branch.data.chunks_exact(frames))
            {
                let [real, imaginary] = *pair else {
                    return Err(bad_shape());
                };
                let restored = stft::inverse(real, imaginary, spectral_frames, frames)?;
                stems.extend(time.iter().zip(&restored).map(|(&t, &s)| t + s));
            }
            if stems.len() != time_branch.data.len() {
                return Err(bad_shape());
            }
            println!(
                "host inverse STFT, all stems: {:.1} ms",
                start.elapsed().as_secs_f64() * 1000.0
            );
            Ok(stems)
        }
    }
}

/// Total GPU memory in use, in MiB, as reported by the NVIDIA driver.
fn gpu_memory_used_mib() -> Option<u32> {
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

fn stem_name(index: usize) -> &'static str {
    const NAMES: [&str; 6] = ["drums", "bass", "other", "vocals", "guitar", "piano"];
    NAMES.get(index).copied().unwrap_or("?")
}

/// Per-stem levels and how far the stems are from summing back to the input.
fn report_stems(mix: &[f32], stems: &[f32]) {
    if mix.is_empty() {
        return;
    }
    let non_finite = stems.iter().filter(|s| !s.is_finite()).count();
    println!("final stems, non-finite samples: {non_finite}");
    let mut sum = vec![0.0_f32; mix.len()];
    for (index, stem) in stems.chunks_exact(mix.len()).enumerate() {
        println!("  {:<7} rms {:.4}", stem_name(index), rms(stem));
        for (acc, &sample) in sum.iter_mut().zip(stem) {
            *acc += sample;
        }
    }
    let residual: Vec<f32> = sum.iter().zip(mix).map(|(&s, &m)| s - m).collect();
    println!(
        "  input rms {:.4}; (sum of stems - input) rms {:.4}",
        rms(mix),
        rms(&residual)
    );
}

fn read_samples(path: &Path) -> Result<Vec<f32>, ProbeError> {
    let bytes = fs::read(path).map_err(|source| ProbeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(bytes
        .chunks_exact(4)
        .filter_map(|chunk| <[u8; 4]>::try_from(chunk).ok())
        .map(f32::from_le_bytes)
        .collect())
}

fn write_samples(path: &Path, samples: &[f32]) -> Result<(), ProbeError> {
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    fs::write(path, bytes).map_err(|source| ProbeError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn compare(reference: &[f32], output: &[f32]) -> Result<(), ProbeError> {
    if reference.len() != output.len() {
        return Err(ProbeError::CompareLength {
            reference: reference.len(),
            output: output.len(),
        });
    }
    let diff: Vec<f32> = reference.iter().zip(output).map(|(&r, &o)| r - o).collect();
    let max = diff.iter().fold(0.0_f32, |acc, d| acc.max(d.abs()));
    println!(
        "vs reference: max |diff| {max:.6}, diff rms {:.6}, reference rms {:.4}",
        rms(&diff),
        rms(reference)
    );
    Ok(())
}

fn run_once(
    session: &mut Session,
    inputs: &[InputTensor],
    output_names: &[String],
) -> Result<(f64, Vec<OutputTensor>), ProbeError> {
    let mut feed = Vec::with_capacity(inputs.len());
    for input in inputs {
        let tensor = TensorRef::from_array_view((input.shape.clone(), input.data.as_slice()))?;
        feed.push((input.name.as_str(), tensor));
    }
    let start = Instant::now();
    let outputs = session.run(feed)?;
    let elapsed = start.elapsed().as_secs_f64();

    let mut tensors = Vec::with_capacity(output_names.len());
    for name in output_names {
        let value = outputs
            .get(name)
            .ok_or_else(|| ProbeError::MissingOutput(name.clone()))?;
        let (shape, data) = value.try_extract_tensor::<f32>()?;
        tensors.push(OutputTensor {
            name: name.clone(),
            shape: shape.to_vec(),
            data: data.to_vec(),
        });
    }
    Ok((elapsed, tensors))
}

fn main() -> Result<(), ProbeError> {
    let args = Args::parse(env::args().skip(1))?;
    println!("model: {}", args.model.display());
    println!(
        "device: {:?}, tuning: {:?}, cpu fallback: {:?}, threads: {:?}",
        args.device, args.tuning, args.fallback, args.threads
    );

    let memory_before = gpu_memory_used_mib();
    let load_start = Instant::now();
    let mut session = build_session(&args)?;
    println!(
        "session created in {:.2} s",
        load_start.elapsed().as_secs_f64()
    );

    for input in session.inputs() {
        println!("input: `{}` {:?}", input.name(), input.dtype());
    }
    for output in session.outputs() {
        println!("output: `{}` {:?}", output.name(), output.dtype());
    }
    let contract = Contract::detect(&session)?;
    let output_names: Vec<String> = session
        .outputs()
        .iter()
        .map(|output| output.name().to_owned())
        .collect();

    let mut mix = synth_mix(SEGMENT_FRAMES);
    normalize(&mut mix);
    let inputs = build_inputs(&contract, &mix)?;
    let memory_loaded = gpu_memory_used_mib();

    let mut seconds = Vec::with_capacity(args.runs.get());
    let mut last = Vec::new();
    for run in 1..=args.runs.get() {
        let (elapsed, tensors) = run_once(&mut session, &inputs, &output_names)?;
        println!("run {run}: {elapsed:.3} s");
        seconds.push(elapsed);
        last = tensors;
    }
    let memory_after = gpu_memory_used_mib();

    seconds.sort_by(f64::total_cmp);
    if let (Some(best), Some(median)) = (seconds.first(), seconds.get(seconds.len() / 2)) {
        let audio_seconds = count(SEGMENT_FRAMES) / f64::from(SAMPLE_RATE_HZ);
        println!(
            "chunk of {audio_seconds:.1} s audio: best {best:.3} s, median {median:.3} s, \
             median speed {:.2}x realtime",
            audio_seconds / median
        );
    }
    println!(
        "gpu memory used, MiB: before {memory_before:?}, session loaded {memory_loaded:?}, after runs {memory_after:?}"
    );

    for output in &last {
        println!(
            "raw output `{}` {:?}: rms {:.4}",
            output.name,
            output.shape,
            rms(&output.data)
        );
    }
    let stems = final_stems(&contract, &last, SEGMENT_FRAMES)?;
    report_stems(&mix, &stems);

    if let Some(path) = &args.compare {
        compare(&read_samples(path)?, &stems)?;
    }
    if let Some(path) = &args.dump {
        write_samples(path, &stems)?;
        println!("final stems written to {}", path.display());
    }
    Ok(())
}
