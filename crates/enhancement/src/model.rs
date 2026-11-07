//! Pinned MDX-Net graph descriptions.
use crate::{EnhancerId, MdxParams, NetworkOutput};
use hex_literal::hex;
use stem_split_audio::domain::{ByteCount, Sha256Digest};
use stem_split_inference::{FileSpec, Repository};
/// A cleanup network: its file and how the host prepares the audio for it
/// and turns its output back into audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnhancerSpec {
    file: FileSpec,
    params: MdxParams,
    output: NetworkOutput,
}

impl EnhancerSpec {
    #[must_use]
    pub const fn file(&self) -> &FileSpec {
        &self.file
    }

    #[must_use]
    pub const fn params(&self) -> MdxParams {
        self.params
    }

    /// What the network outputs, which decides how it cleans each stem and
    /// whether it can clean a mix.
    #[must_use]
    pub const fn output(&self) -> NetworkOutput {
        self.output
    }
}

/// `Reverb_HQ_By_FoxJoy`. The parameters are those of its entry in the
/// model table UVR publishes (`model_data_new.json`, keyed by the MD5 of the
/// last 10 240 000 bytes of the file, here
/// `cd5b2989ad863f116c855db1dfe24e39`): an FFT of 6144 with a hop of 1024,
/// 3072 bins by 512 frames, compensation 1.035, primary stem "Reverb".
static REVERB_HQ: EnhancerSpec = EnhancerSpec {
    file: FileSpec::new(
        Repository::UvrResources,
        "models/MDXNet",
        "Reverb_HQ_By_FoxJoy",
        ByteCount::new(66_780_123),
        Sha256Digest::new(hex!(
            "233bb5c6aaa365e568659a0a81211746fa881f8f47f82d9e864fce1f7692db80"
        )),
    ),
    params: MdxParams::new(6144, 1024, 3072, 512, 1.035),
    output: NetworkOutput::Unwanted,
};

/// `UVR-MDX-NET-Voc_FT`. Its entry in the same table (hash
/// `77d07b2667ddf05b9e3175941b4454a0`): an FFT of 7680 with a hop of 1024,
/// 3072 bins by 256 frames, compensation 1.021, primary stem "Vocals".
/// Chosen over `Kim_Vocal_2`, which has the same shape, because it takes
/// less out of stems that have no vocals; see stage 9a of the plan.
static VOCAL_BLEED: EnhancerSpec = EnhancerSpec {
    file: FileSpec::new(
        Repository::UvrResources,
        "models/MDXNet",
        "UVR-MDX-NET-Voc_FT",
        ByteCount::new(66_762_490),
        Sha256Digest::new(hex!(
            "534b2070fcc7df514b13ef660dc8cbb328679c2374d04354a5c42bb14ecce111"
        )),
    ),
    params: MdxParams::new(7680, 1024, 3072, 256, 1.021),
    output: NetworkOutput::Vocals,
};

impl EnhancerId {
    /// The file of the network and how to run it.
    #[must_use]
    pub const fn spec(self) -> &'static EnhancerSpec {
        match self {
            Self::ReverbHq => &REVERB_HQ,
            Self::VocalBleed => &VOCAL_BLEED,
        }
    }
}
