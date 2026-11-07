use std::{
    ffi::OsStr,
    fs::File,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use symphonia::core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    errors::Error as SymphoniaError,
    formats::{FormatOptions, FormatReader, TrackType, probe::Hint},
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
};

use super::{DecodedAudio, MAX_DURATION};
use crate::domain::{InputFile, SampleRate, StereoFrames};

/// Why a file could not be turned into audio.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("cannot open `{}`: {source}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("`{}` is not in a recognized audio format: {source}", path.display())]
    Unrecognized {
        path: PathBuf,
        #[source]
        source: SymphoniaError,
    },
    #[error("`{}` contains no audio track", path.display())]
    NoAudioTrack { path: PathBuf },
    #[error("the audio codec of `{}` is not supported: {source}", path.display())]
    UnsupportedCodec {
        path: PathBuf,
        #[source]
        source: SymphoniaError,
    },
    #[error("`{}` is damaged and cannot be read: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: SymphoniaError,
    },
    #[error(
        "`{}` has {channels} channels; only mono and stereo are supported",
        path.display()
    )]
    UnsupportedChannels { path: PathBuf, channels: usize },
    #[error("`{}` declares a sample rate of zero", path.display())]
    InvalidSampleRate { path: PathBuf },
    #[error("`{}` changes its sample rate or channel count midway", path.display())]
    FormatChanged { path: PathBuf },
    #[error("`{}` is longer than the {} h limit", path.display(), limit.as_secs() / 3600)]
    TooLong { path: PathBuf, limit: Duration },
    #[error("`{}` contains no audio", path.display())]
    Empty { path: PathBuf },
}

/// Reads the whole file into memory as stereo at its own sample rate.
///
/// Mono is duplicated into both channels. Encoder delay and padding are
/// trimmed, so the audio lines up with what a player would output.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or decoded, has more than
/// two channels, is longer than [`MAX_DURATION`], or holds no audio at all.
pub fn decode(input: &InputFile) -> Result<DecodedAudio, DecodeError> {
    let path = input.path();
    let (mut format, mut decoder, track_id) = open(path)?;

    let mut collector = Collector::new(path);
    let mut interleaved = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            // A reset means a new logical stream starts (chained Ogg); the
            // first one is the audio the file is about.
            Ok(None) | Err(SymphoniaError::ResetRequired) => break,
            Err(source) => {
                return Err(DecodeError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if packet.track_id != track_id {
            continue;
        }
        let buffer = match decoder.decode(&packet) {
            Ok(buffer) => buffer,
            // One damaged packet is common in real files and costs a few
            // milliseconds of audio; players skip it too.
            Err(source @ (SymphoniaError::DecodeError(_) | SymphoniaError::IoError(_))) => {
                log::warn!(
                    "skipping a damaged packet in `{}`: {source}",
                    path.display()
                );
                continue;
            }
            Err(source) => {
                return Err(DecodeError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };

        let spec = buffer.spec();
        let layout = Layout::parse(spec.rate(), spec.channels().count(), path)?;
        buffer.copy_to_vec_interleaved(&mut interleaved);
        collector.push(layout, &interleaved)?;
    }
    collector.finish()
}

/// How long the audio in `input` is, as its container states it, found
/// without decoding anything.
///
/// `None` when the container does not state it or the file cannot be
/// opened. The length is meant for planning only; [`decode`] reports what
/// is wrong with a file.
#[must_use]
pub fn stated_length(input: &InputFile) -> Option<Duration> {
    let (format, _, track_id) = open(input.path()).ok()?;
    let track = format.tracks().iter().find(|track| track.id == track_id)?;
    let frames = track.num_frames?;
    let rate = track.codec_params.as_ref()?.audio()?.sample_rate?;
    let nanoseconds = u128::from(frames).saturating_mul(1_000_000_000) / u128::from(rate.max(1));
    Some(Duration::from_nanos(
        u64::try_from(nanoseconds).unwrap_or(u64::MAX),
    ))
}

type Opened = (Box<dyn FormatReader>, Box<dyn AudioDecoder>, u32);

/// Opens the container and creates a decoder for its default audio track.
fn open(path: &Path) -> Result<Opened, DecodeError> {
    let file = File::open(path).map_err(|source| DecodeError::Open {
        path: path.to_path_buf(),
        source,
    })?;
    let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(OsStr::to_str) {
        hint.with_extension(extension);
    }

    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|source| DecodeError::Unrecognized {
            path: path.to_path_buf(),
            source,
        })?;

    let no_audio_track = || DecodeError::NoAudioTrack {
        path: path.to_path_buf(),
    };
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(no_audio_track)?;
    let parameters = track
        .codec_params
        .as_ref()
        .and_then(|parameters| parameters.audio())
        .ok_or_else(no_audio_track)?;
    let decoder = symphonia::default::get_codecs()
        .make_audio_decoder(parameters, &AudioDecoderOptions::default())
        .map_err(|source| DecodeError::UnsupportedCodec {
            path: path.to_path_buf(),
            source,
        })?;
    let track_id = track.id;
    Ok((format, decoder, track_id))
}

/// Channel layouts the pipeline accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Channels {
    Mono,
    Stereo,
}

/// Sample rate and channel layout of a decoded packet, already checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    rate: SampleRate,
    channels: Channels,
}

impl Layout {
    fn parse(rate: u32, channels: usize, path: &Path) -> Result<Self, DecodeError> {
        let rate = SampleRate::from_hertz(rate).ok_or_else(|| DecodeError::InvalidSampleRate {
            path: path.to_path_buf(),
        })?;
        let channels = match channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            channels => {
                return Err(DecodeError::UnsupportedChannels {
                    path: path.to_path_buf(),
                    channels,
                });
            }
        };
        Ok(Self { rate, channels })
    }
}

/// Gathers decoded packets into stereo frames, checking that they agree.
struct Collector<'a> {
    path: &'a Path,
    layout: Option<Layout>,
    frames: Vec<[f32; 2]>,
}

impl<'a> Collector<'a> {
    const fn new(path: &'a Path) -> Self {
        Self {
            path,
            layout: None,
            frames: Vec::new(),
        }
    }

    /// Appends one packet of samples interleaved by channel.
    fn push(&mut self, layout: Layout, interleaved: &[f32]) -> Result<(), DecodeError> {
        match self.layout {
            None => self.layout = Some(layout),
            Some(first) if first == layout => {}
            Some(_) => {
                return Err(DecodeError::FormatChanged {
                    path: self.path.to_path_buf(),
                });
            }
        }

        match layout.channels {
            Channels::Mono => self
                .frames
                .extend(interleaved.iter().map(|&sample| [sample, sample])),
            Channels::Stereo => self
                .frames
                .extend_from_slice(interleaved.as_chunks::<2>().0),
        }

        let limit = u64::from(layout.rate.hertz().get()).saturating_mul(MAX_DURATION.as_secs());
        // A count too large for `u64` is certainly over the limit.
        let frames = u64::try_from(self.frames.len()).unwrap_or(u64::MAX);
        if frames > limit {
            return Err(DecodeError::TooLong {
                path: self.path.to_path_buf(),
                limit: MAX_DURATION,
            });
        }
        Ok(())
    }

    fn finish(self) -> Result<DecodedAudio, DecodeError> {
        match self.layout {
            Some(layout) if !self.frames.is_empty() => Ok(DecodedAudio::new(
                layout.rate,
                StereoFrames::new(self.frames),
            )),
            Some(_) | None => Err(DecodeError::Empty {
                path: self.path.to_path_buf(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs};

    use hound::{SampleFormat, WavSpec};

    use super::*;

    const fn spec(channels: u16, sample_rate: u32, bits: u16, format: SampleFormat) -> WavSpec {
        WavSpec {
            channels,
            sample_rate,
            bits_per_sample: bits,
            sample_format: format,
        }
    }

    #[test]
    fn stereo_float_wav_is_read_exactly() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("stereo.wav");
        let frames = [[0.0_f32, 0.5], [-0.25, 1.0], [0.125, -1.0]];
        let mut writer = hound::WavWriter::create(&path, spec(2, 48_000, 32, SampleFormat::Float))?;
        for sample in frames.as_flattened() {
            writer.write_sample(*sample)?;
        }
        writer.finalize()?;

        let audio = decode(&InputFile::new(&path)?)?;

        assert_eq!(audio.rate(), SampleRate::from_hertz(48_000).ok_or("rate")?);
        assert_eq!(audio.frames().as_slice(), frames);
        Ok(())
    }

    #[test]
    fn length_is_read_from_the_header_and_missing_for_unreadable_files()
    -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("two seconds.wav");
        let mut writer = hound::WavWriter::create(&path, spec(1, 8_000, 16, SampleFormat::Int))?;
        for _ in 0..16_000 {
            writer.write_sample(0_i16)?;
        }
        writer.finalize()?;
        let broken = directory.path().join("broken.wav");
        fs::write(&broken, b"not a wav file")?;

        assert_eq!(
            stated_length(&InputFile::new(&path)?),
            Some(Duration::from_secs(2))
        );
        assert_eq!(stated_length(&InputFile::new(&broken)?), None);
        Ok(())
    }

    #[test]
    fn mono_is_duplicated_into_both_channels() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("mono.wav");
        let mut writer = hound::WavWriter::create(&path, spec(1, 22_050, 16, SampleFormat::Int))?;
        for sample in [0_i16, 16_384, -16_384] {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;

        let audio = decode(&InputFile::new(&path)?)?;

        assert_eq!(audio.rate(), SampleRate::from_hertz(22_050).ok_or("rate")?);
        // 16-bit samples scale by 1/32768, so these values are exact.
        assert_eq!(
            audio.frames().as_slice(),
            [[0.0, 0.0], [0.5, 0.5], [-0.5, -0.5]]
        );
        Ok(())
    }

    #[test]
    fn more_than_two_channels_are_rejected() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("surround.wav");
        let mut writer = hound::WavWriter::create(&path, spec(6, 44_100, 16, SampleFormat::Int))?;
        for sample in [0_i16; 12] {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;

        assert!(matches!(
            decode(&InputFile::new(&path)?),
            Err(DecodeError::UnsupportedChannels { channels: 6, .. })
        ));
        Ok(())
    }

    #[test]
    fn file_without_audio_frames_is_rejected() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("silent.wav");
        hound::WavWriter::create(&path, spec(2, 44_100, 16, SampleFormat::Int))?.finalize()?;

        assert!(matches!(
            decode(&InputFile::new(&path)?),
            Err(DecodeError::Empty { .. })
        ));
        Ok(())
    }

    #[test]
    fn file_that_is_not_audio_is_rejected() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("notes.mp3");
        fs::write(&path, "these are lyrics, not sound\n".repeat(200))?;

        assert!(matches!(
            decode(&InputFile::new(&path)?),
            Err(DecodeError::Unrecognized { .. })
        ));
        Ok(())
    }

    #[test]
    fn file_removed_after_selection_is_reported() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("gone.wav");
        fs::write(&path, b"")?;
        let input = InputFile::new(&path)?;
        fs::remove_file(&path)?;

        assert!(matches!(decode(&input), Err(DecodeError::Open { .. })));
        Ok(())
    }

    #[test]
    fn layout_rejects_a_zero_sample_rate_and_extra_channels() -> Result<(), DecodeError> {
        let path = Path::new("song.wav");

        assert_eq!(Layout::parse(44_100, 1, path)?.channels, Channels::Mono);
        assert_eq!(Layout::parse(44_100, 2, path)?.channels, Channels::Stereo);
        assert!(matches!(
            Layout::parse(0, 2, path),
            Err(DecodeError::InvalidSampleRate { .. })
        ));
        for channels in [0, 3, 6] {
            assert!(matches!(
                Layout::parse(44_100, channels, path),
                Err(DecodeError::UnsupportedChannels { channels: reported, .. })
                    if reported == channels
            ));
        }
        Ok(())
    }

    #[test]
    fn collector_rejects_a_change_of_layout() -> Result<(), DecodeError> {
        let path = Path::new("song.ogg");
        let mut collector = Collector::new(path);

        collector.push(Layout::parse(44_100, 2, path)?, &[0.0, 0.0])?;
        collector.push(Layout::parse(44_100, 2, path)?, &[0.0, 0.0])?;
        assert!(matches!(
            collector.push(Layout::parse(44_100, 1, path)?, &[0.0]),
            Err(DecodeError::FormatChanged { .. })
        ));
        assert!(matches!(
            collector.push(Layout::parse(48_000, 2, path)?, &[0.0, 0.0]),
            Err(DecodeError::FormatChanged { .. })
        ));
        Ok(())
    }

    #[test]
    fn collector_enforces_the_duration_limit() -> Result<(), DecodeError> {
        let path = Path::new("endless.wav");
        // At 1 Hz the limit is one frame per second of `MAX_DURATION`.
        let layout = Layout::parse(1, 1, path)?;
        let limit = usize::try_from(MAX_DURATION.as_secs()).unwrap_or(usize::MAX);
        let mut collector = Collector::new(path);

        collector.push(layout, &vec![0.0; limit])?;
        assert!(matches!(
            collector.push(layout, &[0.0]),
            Err(DecodeError::TooLong { limit, .. }) if limit == MAX_DURATION
        ));
        Ok(())
    }

    #[test]
    fn collector_without_frames_reports_an_empty_file() -> Result<(), DecodeError> {
        let path = Path::new("empty.flac");
        assert!(matches!(
            Collector::new(path).finish(),
            Err(DecodeError::Empty { .. })
        ));

        let mut collector = Collector::new(path);
        collector.push(Layout::parse(44_100, 2, path)?, &[])?;
        assert!(matches!(collector.finish(), Err(DecodeError::Empty { .. })));
        Ok(())
    }
}
