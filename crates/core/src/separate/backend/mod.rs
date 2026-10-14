//! Running the model graphs through ONNX Runtime.
//!
//! Every graph of the supported models follows one contract, described by the
//! constants below: the host supplies the waveform and its spectrogram, and
//! adds the inverse transform of the spectral output to the time-domain one.

mod providers;

use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    thread,
};

use ort::{session::Session, value::TensorRef};

use super::{
    CHANNELS, ChunkStems, SEGMENT_FRAMES, Segment, Separator,
    stft::{self, BINS, Stft, StftError},
};
use crate::{
    closed_enum,
    domain::{Stem, StemLayout},
};

/// Input: normalized waveform, `[1, 2, SEGMENT_FRAMES]`.
const MIX: &str = "mix";
/// Input: spectrogram as "complex as channels", `[1, 4, BINS, STFT_FRAMES]`,
/// planes ordered left real, left imaginary, right real, right imaginary.
const MAG: &str = "mag";
/// Output: time-domain branch, `[1, stems, 2, SEGMENT_FRAMES]`.
const WAV: &str = "wav";
/// Output: spectral branch, `[1, stems, 4, BINS, STFT_FRAMES]`, planes as in `MAG`.
const SPEC: &str = "spec";

/// Spectrogram frames of one segment.
const STFT_FRAMES: usize = stft::frames_for(SEGMENT_FRAMES);
/// Values in one spectrogram plane.
const PLANE: usize = BINS * STFT_FRAMES;
/// Real and imaginary plane for each channel.
const PLANES: usize = 2 * CHANNELS;

// The same sizes as tensor dimensions. A test keeps the two sets in step.
const SEGMENT_DIM: i64 = 343_980;
const BINS_DIM: i64 = 2048;
const STFT_FRAMES_DIM: i64 = 336;

/// A stem may be this many times louder than the mix it came from before the
/// result is treated as garbage. Real stems stay near or below the mix level;
/// the failure this guards against overshoots by five orders of magnitude.
const PLAUSIBLE_GAIN: f64 = 10.0;
/// Added to the limit so that near-silent segments, where the model's own
/// noise floor dominates, are not rejected.
const PLAUSIBLE_FLOOR: f64 = 1.0;

closed_enum! {
    /// Which device the user wants the model to run on.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum DevicePreference {
        /// The GPU if it works, otherwise the CPU.
        #[default]
        Auto,
        /// The GPU; fail if it cannot be used.
        Gpu,
        Cpu,
    }
}

/// The device a model actually runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveDevice {
    /// A GPU through `DirectML` on Windows or `CoreML` on macOS.
    DirectMl,
    Cpu,
}

/// Why a model could not be loaded or run.
#[derive(Debug, thiserror::Error)]
pub enum InferenceError {
    #[error("cannot load the model `{}`: {source}", path.display())]
    Load {
        path: PathBuf,
        #[source]
        source: Box<ort::Error>,
    },
    #[error("GPU acceleration is not available on this platform")]
    GpuUnsupported,
    #[error("cannot run the model `{}` on the GPU: {source}", path.display())]
    GpuUnavailable {
        path: PathBuf,
        #[source]
        source: Box<ort::Error>,
    },
    #[error("the model `{}` has no tensor named `{tensor}`", path.display())]
    MissingTensor { path: PathBuf, tensor: &'static str },
    #[error("the model run failed: {0}")]
    Run(#[source] Box<ort::Error>),
    #[error("tensor `{tensor}` has the unexpected shape {shape:?}")]
    OutputShape {
        tensor: &'static str,
        shape: Vec<i64>,
    },
    #[error("the model output has {available} stems, stem {index} was requested")]
    MissingStem { index: usize, available: usize },
    #[error("a graph is set up to supply the {stem:?} stem, which the model does not have")]
    StemNotInLayout { stem: Stem },
    #[error(transparent)]
    Stft(#[from] StftError),
    #[error("the model produced implausible audio on {device:?}")]
    Implausible { device: ActiveDevice },
}

impl From<ort::Error> for InferenceError {
    fn from(error: ort::Error) -> Self {
        Self::Run(Box::new(error))
    }
}

/// Whether a CPU session may take over when the GPU misbehaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fallback {
    ToCpu,
    Unavailable,
}

/// How many threads a CPU session may use; at least one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuThreads(NonZeroUsize);

impl CpuThreads {
    #[must_use]
    pub const fn new(threads: NonZeroUsize) -> Self {
        Self(threads)
    }

    /// Every logical core. On the machine the settings were measured on,
    /// that is about a tenth faster than ONNX Runtime's default of one
    /// thread per physical core.
    #[must_use]
    pub fn all() -> Self {
        Self(thread::available_parallelism().unwrap_or(NonZeroUsize::MIN))
    }

    /// An equal share of the cores for each of `sessions` running at once,
    /// so that together they do not ask for more threads than there are
    /// cores.
    #[must_use]
    pub fn share(sessions: NonZeroUsize) -> Self {
        Self::all().divided_by(sessions)
    }

    fn divided_by(self, sessions: NonZeroUsize) -> Self {
        Self(NonZeroUsize::new(self.0.get() / sessions.get()).unwrap_or(NonZeroUsize::MIN))
    }

    #[must_use]
    pub const fn get(self) -> NonZeroUsize {
        self.0
    }
}

/// One model graph loaded into ONNX Runtime.
#[derive(Debug)]
pub struct OrtSeparator {
    session: Session,
    device: ActiveDevice,
    fallback: Fallback,
    graph: PathBuf,
    /// For a CPU session, including one that replaces a failed GPU.
    threads: CpuThreads,
    stems: NonZeroUsize,
    stft: Stft,
    /// Model input `MIX`: left channel, then right.
    mix: Vec<f32>,
    /// Model input `MAG`.
    spectrogram: Vec<f32>,
    /// Inverse transform of one channel of one stem.
    restored: Vec<f32>,
}

impl OrtSeparator {
    /// Loads the graph at `graph`, which produces the stems of `layout`.
    /// On the CPU it runs on `threads` threads.
    ///
    /// Under [`DevicePreference::Auto`] a GPU that cannot be initialized is
    /// replaced by the CPU right here, and one that fails or returns garbage
    /// later is replaced when that happens.
    ///
    /// # Errors
    ///
    /// Returns an error if no session can be created on a device the
    /// preference allows, or if the graph does not have the expected tensors.
    pub fn load(
        graph: &Path,
        layout: StemLayout,
        preference: DevicePreference,
        threads: CpuThreads,
    ) -> Result<Self, InferenceError> {
        let on_cpu = || {
            providers::cpu_session(graph, threads).map_err(|source| InferenceError::Load {
                path: graph.to_path_buf(),
                source: Box::new(source),
            })
        };
        let (session, device, fallback) = match preference {
            DevicePreference::Cpu => (on_cpu()?, ActiveDevice::Cpu, Fallback::Unavailable),
            DevicePreference::Gpu => match providers::gpu_session(graph) {
                Some(Ok(session)) => (session, ActiveDevice::DirectMl, Fallback::Unavailable),
                Some(Err(source)) => {
                    return Err(InferenceError::GpuUnavailable {
                        path: graph.to_path_buf(),
                        source: Box::new(source),
                    });
                }
                None => return Err(InferenceError::GpuUnsupported),
            },
            DevicePreference::Auto => match providers::gpu_session(graph) {
                Some(Ok(session)) => (session, ActiveDevice::DirectMl, Fallback::ToCpu),
                Some(Err(error)) => {
                    log::warn!("GPU session could not be created, using the CPU: {error}");
                    (on_cpu()?, ActiveDevice::Cpu, Fallback::Unavailable)
                }
                None => (on_cpu()?, ActiveDevice::Cpu, Fallback::Unavailable),
            },
        };
        check_contract(&session, graph)?;

        Ok(Self {
            session,
            device,
            fallback,
            graph: graph.to_path_buf(),
            threads,
            stems: layout.count(),
            stft: Stft::new(),
            mix: Vec::with_capacity(CHANNELS * SEGMENT_FRAMES),
            spectrogram: vec![0.0; PLANES * PLANE],
            restored: vec![0.0; SEGMENT_FRAMES],
        })
    }

    /// Runs the graph on the current device and checks the result.
    fn attempt(&mut self, segment: &Segment) -> Result<ChunkStems, InferenceError> {
        let stems = self.infer(segment)?;
        if is_plausible(segment, &stems) {
            Ok(stems)
        } else {
            Err(InferenceError::Implausible {
                device: self.device,
            })
        }
    }

    fn infer(&mut self, segment: &Segment) -> Result<ChunkStems, InferenceError> {
        let Self {
            session,
            stems,
            stft,
            mix,
            spectrogram,
            restored,
            ..
        } = self;

        mix.clear();
        mix.extend_from_slice(segment.left());
        mix.extend_from_slice(segment.right());

        let mut planes = spectrogram.as_chunks_mut::<PLANE>().0.iter_mut();
        let (Some(left_real), Some(left_imaginary), Some(right_real), Some(right_imaginary)) =
            (planes.next(), planes.next(), planes.next(), planes.next())
        else {
            return Err(StftError::PlaneSize {
                expected: PLANES * PLANE,
                actual: spectrogram.len(),
            }
            .into());
        };
        stft.forward(segment.left(), left_real, left_imaginary)?;
        stft.forward(segment.right(), right_real, right_imaginary)?;

        let mix_tensor = TensorRef::from_array_view(([1, 2, SEGMENT_DIM], mix.as_slice()))?;
        let spectrogram_tensor = TensorRef::from_array_view((
            [1, 4, BINS_DIM, STFT_FRAMES_DIM],
            spectrogram.as_slice(),
        ))?;
        let outputs = session.run(ort::inputs![MIX => mix_tensor, MAG => spectrogram_tensor])?;

        let stem_dim = i64::try_from(stems.get()).unwrap_or(i64::MAX);
        let (shape, wav) = outputs
            .get(WAV)
            .ok_or(InferenceError::OutputShape {
                tensor: WAV,
                shape: Vec::new(),
            })?
            .try_extract_tensor::<f32>()?;
        if **shape != [1, stem_dim, 2, SEGMENT_DIM] {
            return Err(InferenceError::OutputShape {
                tensor: WAV,
                shape: shape.to_vec(),
            });
        }
        let (shape, spectral) = outputs
            .get(SPEC)
            .ok_or(InferenceError::OutputShape {
                tensor: SPEC,
                shape: Vec::new(),
            })?
            .try_extract_tensor::<f32>()?;
        if **shape != [1, stem_dim, 4, BINS_DIM, STFT_FRAMES_DIM] {
            return Err(InferenceError::OutputShape {
                tensor: SPEC,
                shape: shape.to_vec(),
            });
        }

        // Both outputs list stems, then channels. A channel is one run of
        // samples in `wav` and a real and an imaginary plane in `spectral`.
        let mut samples = wav.to_vec();
        let (plane_pairs, _) = spectral.as_chunks::<PLANE>();
        let (channel_planes, _) = plane_pairs.as_chunks::<2>();
        for (channel, [real, imaginary]) in samples
            .as_chunks_mut::<SEGMENT_FRAMES>()
            .0
            .iter_mut()
            .zip(channel_planes)
        {
            stft.inverse(real, imaginary, restored)?;
            for (sample, &spectral_part) in channel.iter_mut().zip(restored.iter()) {
                *sample += spectral_part;
            }
        }

        let shape = [1, stem_dim, 2, SEGMENT_DIM];
        ChunkStems::new(*stems, samples).ok_or(InferenceError::OutputShape {
            tensor: WAV,
            shape: shape.to_vec(),
        })
    }
}

impl Separator for OrtSeparator {
    fn separate(&mut self, segment: &Segment) -> Result<ChunkStems, InferenceError> {
        let error = match self.attempt(segment) {
            Ok(stems) => return Ok(stems),
            Err(error) => error,
        };
        match (self.device, self.fallback) {
            (ActiveDevice::DirectMl, Fallback::ToCpu) => {
                log::warn!("the GPU failed, continuing on the CPU: {error}");
                self.session =
                    providers::cpu_session(&self.graph, self.threads).map_err(|source| {
                        InferenceError::Load {
                            path: self.graph.clone(),
                            source: Box::new(source),
                        }
                    })?;
                self.device = ActiveDevice::Cpu;
                self.fallback = Fallback::Unavailable;
                self.attempt(segment)
            }
            (
                ActiveDevice::DirectMl | ActiveDevice::Cpu,
                Fallback::ToCpu | Fallback::Unavailable,
            ) => Err(error),
        }
    }

    fn device(&self) -> ActiveDevice {
        self.device
    }
}

fn check_contract(session: &Session, graph: &Path) -> Result<(), InferenceError> {
    let missing = |tensor| InferenceError::MissingTensor {
        path: graph.to_path_buf(),
        tensor,
    };
    for tensor in [MIX, MAG] {
        if !session.inputs().iter().any(|input| input.name() == tensor) {
            return Err(missing(tensor));
        }
    }
    for tensor in [WAV, SPEC] {
        if !session
            .outputs()
            .iter()
            .any(|output| output.name() == tensor)
        {
            return Err(missing(tensor));
        }
    }
    Ok(())
}

#[expect(
    clippy::cast_precision_loss,
    reason = "sample counts are far below 2^52"
)]
fn rms(samples: impl Iterator<Item = f32>) -> f64 {
    let (energy, count) = samples.fold((0.0_f64, 0_usize), |(energy, count), sample| {
        (energy + f64::from(sample).powi(2), count + 1)
    });
    if count == 0 {
        0.0
    } else {
        (energy / count as f64).sqrt()
    }
}

/// Whether `stems` could be the result of separating `segment`: every value
/// finite and no stem far louder than the mix.
fn is_plausible(segment: &Segment, stems: &ChunkStems) -> bool {
    let mix = rms(segment.left().iter().chain(segment.right()).copied());
    let limit = PLAUSIBLE_GAIN * mix + PLAUSIBLE_FLOOR;
    stems
        .samples()
        .as_chunks::<{ CHANNELS * SEGMENT_FRAMES }>()
        .0
        .iter()
        .all(|stem| {
            // A non-finite sample makes the level NaN, which fails the comparison.
            rms(stem.iter().copied()) <= limit
        })
}

#[cfg(test)]
mod tests {
    use std::{error::Error, path::PathBuf};

    use super::*;

    const TWO: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);

    #[test]
    fn tensor_dimensions_match_the_sizes_they_describe() {
        assert_eq!(usize::try_from(SEGMENT_DIM), Ok(SEGMENT_FRAMES));
        assert_eq!(usize::try_from(BINS_DIM), Ok(BINS));
        assert_eq!(usize::try_from(STFT_FRAMES_DIM), Ok(STFT_FRAMES));
        assert_eq!(STFT_FRAMES, 336);
    }

    fn segment_of(level: f32) -> Segment {
        let mut segment = Segment::silent();
        let (left, right) = segment.channels_mut();
        left.fill(level);
        right.fill(-level);
        segment
    }

    fn stems_of(levels: [f32; 2]) -> Option<ChunkStems> {
        let samples = levels
            .into_iter()
            .flat_map(|level| std::iter::repeat_n(level, CHANNELS * SEGMENT_FRAMES))
            .collect();
        ChunkStems::new(TWO, samples)
    }

    #[test]
    fn stems_near_the_mix_level_are_plausible() -> Result<(), &'static str> {
        let segment = segment_of(0.5);
        assert!(is_plausible(
            &segment,
            &stems_of([0.4, 0.1]).ok_or("stems")?
        ));
        assert!(is_plausible(
            &segment,
            &stems_of([0.0, 0.0]).ok_or("stems")?
        ));
        // A stem may exceed the mix when stems cancel each other.
        assert!(is_plausible(
            &segment,
            &stems_of([2.0, -1.5]).ok_or("stems")?
        ));
        Ok(())
    }

    #[test]
    fn runaway_levels_are_implausible() -> Result<(), &'static str> {
        let segment = segment_of(0.5);
        // The level seen when DirectML fused the graph: tens of thousands.
        assert!(!is_plausible(
            &segment,
            &stems_of([0.4, 50_000.0]).ok_or("stems")?
        ));
        assert!(!is_plausible(
            &segment,
            &stems_of([7.0, 0.1]).ok_or("stems")?
        ));
        Ok(())
    }

    #[test]
    fn non_finite_samples_are_implausible() -> Result<(), &'static str> {
        let segment = segment_of(0.5);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut samples = vec![0.1_f32; TWO.get() * CHANNELS * SEGMENT_FRAMES];
            if let Some(sample) = samples.get_mut(123_456) {
                *sample = bad;
            }
            let stems = ChunkStems::new(TWO, samples).ok_or("stems")?;
            assert!(!is_plausible(&segment, &stems), "{bad}");
        }
        Ok(())
    }

    #[test]
    fn quiet_output_for_a_silent_segment_is_plausible() -> Result<(), &'static str> {
        let segment = Segment::silent();
        assert!(is_plausible(
            &segment,
            &stems_of([0.01, -0.02]).ok_or("stems")?
        ));
        assert!(!is_plausible(
            &segment,
            &stems_of([0.01, 300.0]).ok_or("stems")?
        ));
        Ok(())
    }

    #[test]
    fn missing_graph_file_is_a_load_error() {
        let graph = PathBuf::from("no-such-directory-for-stem-split").join("graph.onnx");

        let result = OrtSeparator::load(
            &graph,
            StemLayout::FOUR,
            DevicePreference::Cpu,
            CpuThreads::all(),
        );

        assert!(matches!(
            result,
            Err(InferenceError::Load { path, .. }) if path == graph
        ));
    }

    /// The graph files of the chosen export and of the reference export with
    /// the transforms inside the graph, downloaded during stage 0.
    fn stage_zero_models() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models")
    }

    /// A deterministic segment with bass, harmony and percussive content.
    fn test_segment() -> Segment {
        let mut segment = Segment::silent();
        let (left, right) = segment.channels_mut();
        let mut state = 0x1234_5678_u32;
        let mut envelope = 0.0_f32;
        let mut phase = 0.0_f32;
        for (index, (left, right)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            if index.is_multiple_of(22_050) {
                envelope = 1.0;
            }
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let [_, _, low, high] = state.to_le_bytes();
            let noise = f32::from(u16::from_le_bytes([low, high])) / 32_768.0 - 1.0;
            phase += 0.007_835;
            let bass = 0.8 * phase.sin();
            let chord = 0.2 * ((phase * 8.0).sin() + (phase * 10.08).sin());
            *left = bass + chord + envelope * noise;
            *right = bass - chord + envelope * noise;
            envelope *= 0.999_25;
        }
        segment
    }

    fn worst_difference(first: &ChunkStems, second: &ChunkStems) -> f32 {
        first
            .samples()
            .iter()
            .zip(second.samples())
            .fold(0.0_f32, |worst, (a, b)| worst.max((a - b).abs()))
    }

    /// Runs the reference export, whose graph contains the transforms.
    fn run_reference(graph: &Path, segment: &Segment) -> Result<ChunkStems, Box<dyn Error>> {
        let mut session = providers::cpu_session(graph, CpuThreads::all())?;
        let mix: Vec<f32> = segment
            .left()
            .iter()
            .chain(segment.right())
            .copied()
            .collect();
        let tensor = TensorRef::from_array_view(([1, 2, SEGMENT_DIM], mix.as_slice()))?;
        let outputs = session.run(ort::inputs!["mix" => tensor])?;
        let (shape, data) = outputs
            .get("sources")
            .ok_or("no `sources` output")?
            .try_extract_tensor::<f32>()?;
        let stems = shape
            .get(1)
            .and_then(|&count| usize::try_from(count).ok())
            .and_then(NonZeroUsize::new)
            .ok_or("unexpected shape")?;
        Ok(ChunkStems::new(stems, data.to_vec()).ok_or("unexpected size")?)
    }

    #[test]
    #[ignore = "needs the model files downloaded in stage 0 under models/"]
    fn host_transforms_match_the_export_with_transforms_in_the_graph() -> Result<(), Box<dyn Error>>
    {
        let models = stage_zero_models();
        let segment = test_segment();
        for (name, layout) in [
            ("htdemucs", StemLayout::FOUR),
            ("htdemucs_6s", StemLayout::SIX),
        ] {
            let ours = OrtSeparator::load(
                &models.join(format!("hrsadeghi/{name}.onnx")),
                layout,
                DevicePreference::Cpu,
                CpuThreads::all(),
            )?
            .separate(&segment)?;
            let reference = run_reference(&models.join(format!("smank/{name}.onnx")), &segment)?;

            assert_eq!(ours.stem_count(), reference.stem_count());
            let worst = worst_difference(&ours, &reference);
            assert!(worst < 2e-3, "{name}: worst difference {worst}");
        }
        Ok(())
    }

    #[test]
    #[ignore = "needs a DirectML GPU and the model files downloaded in stage 0"]
    fn gpu_matches_the_cpu() -> Result<(), Box<dyn Error>> {
        let graph = stage_zero_models().join("hrsadeghi/htdemucs.onnx");
        let segment = test_segment();

        let mut gpu = OrtSeparator::load(
            &graph,
            StemLayout::FOUR,
            DevicePreference::Gpu,
            CpuThreads::all(),
        )?;
        let mut cpu = OrtSeparator::load(
            &graph,
            StemLayout::FOUR,
            DevicePreference::Cpu,
            CpuThreads::all(),
        )?;
        assert_eq!(gpu.device(), ActiveDevice::DirectMl);
        assert_eq!(cpu.device(), ActiveDevice::Cpu);

        let accelerated = gpu.separate(&segment)?;
        let reference = cpu.separate(&segment)?;

        let worst = worst_difference(&accelerated, &reference);
        assert!(worst < 2e-3, "worst difference {worst}");
        // The GPU session must not have been swapped for a CPU one.
        assert_eq!(gpu.device(), ActiveDevice::DirectMl);
        Ok(())
    }
}
