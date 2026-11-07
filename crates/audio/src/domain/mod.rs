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
mod output;
mod output_format;
mod stem;
mod stem_set;
mod units;

pub use self::input::comparable;
pub use self::{
    audio_format::AudioFormat,
    digest::{DigestParseError, Sha256Digest},
    frames::{ChannelLengthMismatch, StereoFrames},
    input::{InputFile, InputFileError, InputFiles},
    output::{OutputChoice, OutputDir, OutputDirError, OverwritePolicy},
    output_format::OutputFormat,
    stem::{Stem, StemLayout},
    stem_set::StemSet,
    units::{ByteCount, FrameCount, SampleRate},
};
