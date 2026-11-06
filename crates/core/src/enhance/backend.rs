//! Running an MDX-Net graph through ONNX Runtime.
//!
//! The graph takes the spectrogram of a window as "complex as channels",
//! `[1, 4, bins, frames]` with planes left real, left imaginary, right real,
//! right imaginary, and returns one of the same shape.

use std::path::Path;

use ort::{session::Session, value::TensorRef};

use super::{Enhancer, MdxParams, stft::MdxStft};
use crate::{
    providers::DeviceSession,
    separate::{ActiveDevice, CpuThreads, DevicePreference, InferenceError},
};

const INPUT: &str = "input";
const OUTPUT: &str = "output";
/// Real and imaginary plane for each of the two channels.
const PLANES: usize = 4;
/// The lowest bins UVR silences before the network sees them: they carry
/// the DC offset and rumble, which the networks were not trained on.
const SILENCED_BINS: usize = 3;

/// The output may be this many times louder than the window it came from
/// before it is treated as garbage, as for the separation models.
const PLAUSIBLE_GAIN: f64 = 10.0;
/// Added to the limit so that near-silent windows are not rejected.
const PLAUSIBLE_FLOOR: f64 = 1.0;

/// An MDX-Net graph loaded into ONNX Runtime.
#[derive(Debug)]
pub struct OrtEnhancer {
    runtime: DeviceSession,
    stft: MdxStft,
    /// Dimensions of the input and output tensors.
    shape: [i64; 4],
    /// Spectrogram frames of a window: the length of a row of a plane.
    frames: usize,
    /// One channel of the window, and the restored output of one channel.
    left: Vec<f32>,
    right: Vec<f32>,
    /// Model input, four planes.
    spectrogram: Vec<f32>,
}

impl OrtEnhancer {
    /// Loads the graph at `graph`, fed as `params` describes. On the CPU it
    /// runs on `threads` threads; the device follows `preference` as for the
    /// separation models.
    ///
    /// # Errors
    ///
    /// Returns an error if no session can be created on a device the
    /// preference allows, or if the graph does not have the expected tensors.
    pub fn load(
        graph: &Path,
        params: &MdxParams,
        preference: DevicePreference,
        threads: CpuThreads,
    ) -> Result<Self, InferenceError> {
        let stft = MdxStft::new(params)?;
        let dimension = |value: usize| i64::try_from(value).unwrap_or(i64::MAX);
        let shape = [
            1,
            dimension(PLANES),
            dimension(params.bins()),
            dimension(params.frames()),
        ];
        let runtime = DeviceSession::open(graph, preference, threads)?;
        check_contract(runtime.session(), graph)?;
        Ok(Self {
            runtime,
            shape,
            frames: params.frames(),
            left: vec![0.0; stft.length()],
            right: vec![0.0; stft.length()],
            spectrogram: vec![0.0; PLANES * stft.plane()],
            stft,
        })
    }

    /// Runs the graph on the current device and checks the result.
    fn attempt(&mut self, window: &[[f32; 2]]) -> Result<Vec<[f32; 2]>, InferenceError> {
        let output = self.infer(window)?;
        if is_plausible(window, &output) {
            Ok(output)
        } else {
            Err(InferenceError::Implausible {
                device: self.runtime.device(),
            })
        }
    }

    fn infer(&mut self, window: &[[f32; 2]]) -> Result<Vec<[f32; 2]>, InferenceError> {
        let Self {
            runtime,
            stft,
            shape,
            frames,
            left,
            right,
            spectrogram,
        } = self;
        let plane = stft.plane();
        if window.len() != left.len() {
            return Err(InferenceError::OutputShape {
                tensor: INPUT,
                shape: vec![i64::try_from(window.len()).unwrap_or(i64::MAX)],
            });
        }
        for ((frame, left), right) in window.iter().zip(left.iter_mut()).zip(right.iter_mut()) {
            [*left, *right] = *frame;
        }

        let values = spectrogram.len();
        let mut planes = spectrogram.chunks_exact_mut(plane);
        let (Some(left_real), Some(left_imaginary), Some(right_real), Some(right_imaginary)) =
            (planes.next(), planes.next(), planes.next(), planes.next())
        else {
            return Err(missing_planes(values, plane));
        };
        stft.forward(left, left_real, left_imaginary)?;
        stft.forward(right, right_real, right_imaginary)?;
        // Each plane is `[bins][frames]`: the lowest bins are its first rows.
        for plane in spectrogram.chunks_exact_mut(plane) {
            for sample in plane.iter_mut().take(SILENCED_BINS.saturating_mul(*frames)) {
                *sample = 0.0;
            }
        }

        let input = TensorRef::from_array_view((*shape, spectrogram.as_slice()))?;
        let outputs = runtime.session_mut().run(ort::inputs![INPUT => input])?;
        let (output_shape, data) = outputs
            .get(OUTPUT)
            .ok_or(InferenceError::OutputShape {
                tensor: OUTPUT,
                shape: Vec::new(),
            })?
            .try_extract_tensor::<f32>()?;
        if **output_shape != *shape {
            return Err(InferenceError::OutputShape {
                tensor: OUTPUT,
                shape: output_shape.to_vec(),
            });
        }

        let mut planes = data.chunks_exact(plane);
        let (Some(left_real), Some(left_imaginary), Some(right_real), Some(right_imaginary)) =
            (planes.next(), planes.next(), planes.next(), planes.next())
        else {
            return Err(missing_planes(data.len(), plane));
        };
        stft.inverse(left_real, left_imaginary, left)?;
        stft.inverse(right_real, right_imaginary, right)?;
        Ok(left
            .iter()
            .zip(right.iter())
            .map(|(&left, &right)| [left, right])
            .collect())
    }
}

fn missing_planes(values: usize, plane: usize) -> InferenceError {
    crate::separate::StftError::PlaneSize {
        expected: PLANES.saturating_mul(plane),
        actual: values,
    }
    .into()
}

impl Enhancer for OrtEnhancer {
    fn predict(&mut self, window: &[[f32; 2]]) -> Result<Vec<[f32; 2]>, InferenceError> {
        match self.attempt(window) {
            Ok(output) => Ok(output),
            Err(error) => {
                self.runtime.fall_back(error)?;
                self.attempt(window)
            }
        }
    }

    fn device(&self) -> ActiveDevice {
        self.runtime.device()
    }
}

fn check_contract(session: &Session, graph: &Path) -> Result<(), InferenceError> {
    let missing = |tensor| InferenceError::MissingTensor {
        path: graph.to_path_buf(),
        tensor,
    };
    if !session.inputs().iter().any(|input| input.name() == INPUT) {
        return Err(missing(INPUT));
    }
    if !session
        .outputs()
        .iter()
        .any(|output| output.name() == OUTPUT)
    {
        return Err(missing(OUTPUT));
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

/// Whether `output` could be the network's answer to `window`: every value
/// finite and the level not far above the input's.
fn is_plausible(window: &[[f32; 2]], output: &[[f32; 2]]) -> bool {
    let input = rms(window.as_flattened().iter().copied());
    // A non-finite sample makes the level NaN, which fails the comparison.
    rms(output.as_flattened().iter().copied()) <= PLAUSIBLE_GAIN * input + PLAUSIBLE_FLOOR
}

#[cfg(test)]
mod tests {
    use std::{error::Error, path::PathBuf};

    use super::*;
    use crate::domain::EnhancerId;

    #[test]
    fn plausibility_follows_the_input_level() {
        let window = vec![[0.5_f32, -0.5]; 1000];

        assert!(is_plausible(&window, &vec![[0.4, 0.1]; 1000]));
        assert!(is_plausible(&window, &vec![[0.0, 0.0]; 1000]));
        assert!(!is_plausible(&window, &vec![[50_000.0, 0.0]; 1000]));
        let mut broken = vec![[0.1_f32, 0.1]; 1000];
        if let Some(frame) = broken.get_mut(500) {
            *frame = [f32::NAN, 0.0];
        }
        assert!(!is_plausible(&window, &broken));
    }

    #[test]
    fn missing_graph_file_is_a_load_error() {
        let graph = PathBuf::from("no-such-directory-for-stem-split").join("cleanup.onnx");

        let result = OrtEnhancer::load(
            &graph,
            &EnhancerId::ReverbHq.spec().params(),
            DevicePreference::Cpu,
            CpuThreads::all(),
        );

        assert!(matches!(
            result,
            Err(InferenceError::Load { path, .. }) if path == graph
        ));
    }

    /// Where the probes of stages 8a and 9a put the cleanup graphs, under
    /// the names they have in their repository.
    fn probe_model(id: EnhancerId) -> PathBuf {
        let remote = id.spec().file().remote_path();
        let name = remote.rsplit('/').next().unwrap_or(&remote).to_owned();
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../models/uvr")
            .join(name)
    }

    /// A window of a decaying tone with noise: something to clean.
    #[expect(clippy::cast_precision_loss, reason = "test signal of small integers")]
    fn test_window(length: usize) -> Vec<[f32; 2]> {
        let mut state = 0x1234_5678_u32;
        (0..length)
            .map(|index| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let [_, _, low, high] = state.to_le_bytes();
                let noise = f32::from(u16::from_le_bytes([low, high])) / 32_768.0 - 1.0;
                let time = index as f32 / 44_100.0;
                let tone = (std::f32::consts::TAU * 220.0 * time).sin() * (-time).exp();
                [0.4 * tone + 0.05 * noise, 0.4 * tone - 0.05 * noise]
            })
            .collect()
    }

    #[test]
    #[ignore = "needs a DirectML GPU and the cleanup models under models/uvr"]
    fn gpu_matches_the_cpu() -> Result<(), Box<dyn Error>> {
        for &id in EnhancerId::ALL {
            let params = id.spec().params();
            let graph = probe_model(id);
            let mut gpu =
                OrtEnhancer::load(&graph, &params, DevicePreference::Gpu, CpuThreads::all())?;
            let mut cpu =
                OrtEnhancer::load(&graph, &params, DevicePreference::Cpu, CpuThreads::all())?;
            let window = test_window(params.chunking()?.window);

            let accelerated = gpu.predict(&window)?;
            let reference = cpu.predict(&window)?;

            let worst = accelerated
                .as_flattened()
                .iter()
                .zip(reference.as_flattened())
                .fold(0.0_f32, |worst, (a, b)| worst.max((a - b).abs()));
            assert!(worst < 1e-3, "{}: worst difference {worst}", id.name());
            assert_eq!(gpu.device(), ActiveDevice::DirectMl);
        }
        Ok(())
    }
}
