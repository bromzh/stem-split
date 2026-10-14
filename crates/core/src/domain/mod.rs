//! Values the pipeline passes around, each valid by construction.
//!
//! Data from the outside world (paths, numbers, strings) is checked once, at
//! the constructor of one of these types. Code that receives them does not
//! validate again.

mod audio_format;
mod digest;
mod frames;
mod input;
mod macros;
mod model;
mod output;
mod stem;
mod units;

pub(crate) use self::input::comparable;
pub use self::{
    audio_format::AudioFormat,
    digest::{DigestParseError, Sha256Digest},
    frames::{ChannelLengthMismatch, StereoFrames},
    input::{InputFile, InputFileError, InputFiles},
    model::ModelId,
    output::{OutputChoice, OutputDir, OutputDirError, OverwritePolicy},
    stem::{Stem, StemLayout},
    units::{ByteCount, FrameCount, SampleRate},
};
