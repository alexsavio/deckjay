//! Reads a music file into plain samples ([`Source`]) and fits them to the
//! sound output ([`Converter`]).

use std::fs::File;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, Track, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase};
use tracing::{debug, warn};

/// One audio file, open and ready to decode.
pub(super) struct Source {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    duration: Option<Duration>,
    /// Where decoding begins: zero, or where a seek landed.
    start: Duration,
}

impl Source {
    /// Fails for a file that is missing, is not audio, or uses a codec
    /// symphonia cannot decode (Opus).
    pub(super) fn open(path: &Path) -> Result<Source> {
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(extension);
        }
        let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
        let reader = symphonia::default::get_probe()
            .probe(
                &hint,
                stream,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .context("not a known audio format")?;
        let track = reader
            .default_track(TrackType::Audio)
            .context("no audio track")?;
        let track_id = track.id;
        let time_base = track.time_base;
        let params = track
            .codec_params
            .as_ref()
            .and_then(CodecParameters::audio)
            .context("no audio track")?;
        let duration = length(track, params.sample_rate);
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .context("cannot decode this codec")?;
        Ok(Source {
            reader,
            decoder,
            track_id,
            time_base,
            duration,
            start: Duration::ZERO,
        })
    }

    /// The track's length, when the file tells it.
    pub(super) fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Opens `path` at about `position`. A seek that fails (past the end, or a
    /// file that cannot seek) starts the track from its beginning.
    pub(super) fn open_at(path: &Path, position: Duration) -> Result<Source> {
        let mut source = Source::open(path)?;
        if position.is_zero() {
            return Ok(source);
        }
        match source.seek(position) {
            Ok(_) => Ok(source),
            Err(err) => {
                warn!("{}: {err:#}; playing it from its beginning", path.display());
                Source::open(path)
            }
        }
    }

    /// Where the audio [`next`](Self::next) returns begins in the track.
    pub(super) fn start(&self) -> Duration {
        self.start
    }

    /// Moves to about `to`: a coarse seek lands on a packet near it, before
    /// or after. Fails past the end, or for a file that cannot seek; the
    /// source is then in an unknown place.
    pub(super) fn seek(&mut self, to: Duration) -> Result<Duration> {
        let time = Time::try_from_secs_f64(to.as_secs_f64()).context("not a time to seek to")?;
        let seeked = self
            .reader
            .seek(
                SeekMode::Coarse,
                SeekTo::Time {
                    time,
                    // An m4b may list a chapter or cover track first.
                    track_id: Some(self.track_id),
                },
            )
            .with_context(|| format!("cannot seek to {to:?}"))?;
        // Its state belongs to the packets before the seek.
        self.decoder.reset();
        let landed = self
            .time_base
            .and_then(|base| base.calc_time(seeked.actual_ts))
            .map_or(to.as_secs_f64(), |time| time.as_secs_f64());
        // Encoder delay puts a track's first packet before zero.
        self.start = Duration::try_from_secs_f64(landed.max(0.0)).unwrap_or(to);
        Ok(self.start)
    }

    /// Decodes the next packet into `samples` (interleaved) and returns its
    /// sample rate and channel count; `None` once the track is over.
    pub(super) fn next(&mut self, samples: &mut Vec<f32>) -> Option<(u32, usize)> {
        loop {
            let packet = match self.reader.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return None,
                Err(Error::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => return None,
                Err(err) => {
                    warn!("cannot read the rest of the track: {err}");
                    return None;
                }
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(audio) if audio.frames() > 0 => {
                    let spec = audio.spec();
                    let format = (spec.rate(), spec.channels().count());
                    audio.copy_to_vec_interleaved(samples);
                    return Some(format);
                }
                Ok(_) => {}
                // symphonia's advice: drop the packet and go on with the next.
                Err(err @ (Error::DecodeError(_) | Error::IoError(_))) => {
                    debug!("skipping a bad packet: {err}");
                }
                Err(err) => {
                    warn!("cannot decode the rest of the track: {err}");
                    return None;
                }
            }
        }
    }
}

/// From the container's duration, else from the frame count and `rate`.
fn length(track: &Track, rate: Option<u32>) -> Option<Duration> {
    let stated = track
        .time_base
        .zip(track.duration)
        .and_then(|(base, duration)| base.calc_duration(duration))
        .map(|time| time.as_secs_f64());
    let counted = || Some(track.num_frames? as f64 / f64::from(rate.filter(|r| *r > 0)?));
    let secs = stated.or_else(counted)?;
    Duration::try_from_secs_f64(secs)
        .ok()
        .filter(|d| !d.is_zero())
}

/// Fits decoded audio to the output's sample rate and channels: mono plays on
/// the first two channels, stereo (or the front pair of more) on the first
/// two, and any other channels stay silent. A one-channel output gets the mix
/// of left and right.
///
/// The resampler interpolates linearly: cheap enough for a Raspberry Pi 3,
/// and a rate change is rare (most music is 44.1 kHz, most outputs 44.1 or
/// 48 kHz).
pub(super) struct Converter {
    rate: u32,
    channels: usize,
    /// The input rate the resampler state below belongs to.
    input_rate: u32,
    /// Where the next output frame falls, in input frames after `last`.
    pos: f64,
    last: Option<[f32; 2]>,
}

impl Converter {
    pub(super) fn new(rate: u32, channels: usize) -> Converter {
        Converter {
            rate,
            channels,
            input_rate: rate,
            pos: 0.0,
            last: None,
        }
    }

    /// Appends `input` (interleaved, `channels` per frame, at `rate`) to `out`,
    /// converted. The resampler carries its position across calls, so a
    /// track can come in packet by packet.
    pub(super) fn push(&mut self, input: &[f32], channels: usize, rate: u32, out: &mut Vec<f32>) {
        if channels == 0 || rate == 0 {
            return;
        }
        if rate != self.input_rate {
            self.input_rate = rate;
            self.pos = 0.0;
            self.last = None;
        }
        let step = f64::from(rate) / f64::from(self.rate);
        for frame in input.chunks_exact(channels) {
            let frame = [frame[0], frame[channels.min(2) - 1]];
            if rate == self.rate {
                self.write(frame, out);
                continue;
            }
            let Some(last) = self.last else {
                self.last = Some(frame);
                continue;
            };
            while self.pos < 1.0 {
                let t = self.pos as f32;
                let mixed = [
                    last[0] + (frame[0] - last[0]) * t,
                    last[1] + (frame[1] - last[1]) * t,
                ];
                self.write(mixed, out);
                self.pos += step;
            }
            self.pos -= 1.0;
            self.last = Some(frame);
        }
    }

    fn write(&self, [left, right]: [f32; 2], out: &mut Vec<f32>) {
        if self.channels == 1 {
            out.push(f32::midpoint(left, right));
            return;
        }
        out.push(left);
        out.push(right);
        out.extend(std::iter::repeat_n(0.0, self.channels - 2));
    }
}
