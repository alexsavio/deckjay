//! The part of local playback that needs no sound card: a decoder thread
//! turns the current track into [`Chunk`]s in a lock-free ring, and the sound
//! card's callback plays them with [`Sink::fill`]. Tests call `fill`
//! themselves.
//!
//! Every track gets a new generation number. The player thread sets it before
//! it tells the decoder about the track, and each chunk carries the number of
//! the track it belongs to, so `fill` drops what an older track left in the
//! ring the moment the number changes. After a track's last chunk comes an
//! end marker; `fill` reaching it means the track has been heard to the end.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use cpal::{FromSample, Sample};
use rtrb::{Consumer, Producer, RingBuffer};
use tracing::warn;

use super::decode::{Converter, Source};
use super::netread::Cancel;

/// Samples in one [`Chunk`], for all channels together.
const CHUNK_SAMPLES: usize = 1024;
/// Seconds of decoded audio that wait for the sound card.
const BUFFER_SECS: f64 = 1.5;
/// How long the decoder waits for room when the ring is full.
const FULL_WAIT: Duration = Duration::from_millis(10);
/// Keeps a chunk able to hold whole frames.
const MAX_CHANNELS: usize = 32;

/// What the sound output takes: samples per second and interleaved channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Format {
    pub(super) rate: u32,
    pub(super) channels: usize,
}

/// A piece of one track, ready for the sound card. `len` is always whole
/// frames, so the channels stay in place however the chunks are cut.
#[derive(Clone, Copy)]
struct Chunk {
    generation: u32,
    len: usize,
    /// The marker after a track's last chunk; it holds no samples.
    end: bool,
    samples: [f32; CHUNK_SAMPLES],
}

impl Chunk {
    fn audio(generation: u32, samples: &[f32]) -> Chunk {
        let mut chunk = Chunk::end(generation);
        chunk.end = false;
        chunk.len = samples.len();
        chunk.samples[..samples.len()].copy_from_slice(samples);
        chunk
    }

    fn end(generation: u32) -> Chunk {
        Chunk {
            generation,
            len: 0,
            end: true,
            samples: [0.0; CHUNK_SAMPLES],
        }
    }
}

/// State the player thread and the sound card's callback share.
struct Shared {
    /// The track to play; older chunks are dropped.
    generation: AtomicU32,
    paused: AtomicBool,
    /// An `f32` gain, as bits.
    volume: AtomicU32,
    /// The generation whose end marker `fill` reached last.
    finished: AtomicU32,
    /// Frames played of one generation: the generation in the high 32 bits.
    progress: AtomicU64,
    /// Set by the sound card when the stream breaks; the callback never
    /// touches it.
    error: Mutex<Option<String>>,
}

enum Job {
    Play(u32, Source),
    Stop,
}

/// The player thread's handle on the audio: starts, stops and pauses tracks.
pub(super) struct Engine {
    shared: Arc<Shared>,
    jobs: Sender<Job>,
    decoder: JoinHandle<()>,
    rate: u32,
    generation: u32,
    /// Where the current track's audio begins: zero, or where a seek landed.
    offset: Duration,
    /// The current radio stream's: a decoder thread waiting for the network
    /// would not see the next job.
    cancel: Option<Cancel>,
}

/// The sound card's end: see [`Sink::fill`].
pub(super) struct Sink {
    ring: Consumer<Chunk>,
    shared: Arc<Shared>,
    channels: usize,
    /// Samples of the ring's first chunk already played.
    offset: usize,
    counted: u32,
    frames: u64,
}

/// Lets the sound card report that its stream broke.
#[derive(Clone)]
pub(super) struct Failures(Arc<Shared>);

impl Failures {
    pub(super) fn report(&self, error: String) {
        warn!("the sound output failed: {error}");
        *self.0.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
    }
}

impl Engine {
    /// Starts the decoder thread; the [`Sink`] goes to the sound card.
    pub(super) fn new(format: Format) -> Result<(Engine, Sink)> {
        ensure!(format.rate > 0, "the sound output has no sample rate");
        ensure!(
            (1..=MAX_CHANNELS).contains(&format.channels),
            "the sound output has {} channels",
            format.channels
        );
        let chunk_len = CHUNK_SAMPLES / format.channels * format.channels;
        let buffered = f64::from(format.rate) * BUFFER_SECS * format.channels as f64;
        let (producer, consumer) = RingBuffer::new((buffered / chunk_len as f64).ceil() as usize);
        let shared = Arc::new(Shared {
            generation: AtomicU32::new(0),
            paused: AtomicBool::new(false),
            volume: AtomicU32::new(1.0_f32.to_bits()),
            finished: AtomicU32::new(0),
            progress: AtomicU64::new(0),
            error: Mutex::new(None),
        });
        let (jobs, job_rx) = mpsc::channel();
        let decoder = thread::Builder::new()
            .name("local-decoder".into())
            .spawn(move || decode_jobs(&job_rx, producer, format, chunk_len))
            .context("cannot start the decoder thread")?;
        let engine = Engine {
            shared: Arc::clone(&shared),
            jobs,
            decoder,
            rate: format.rate,
            generation: 0,
            offset: Duration::ZERO,
            cancel: None,
        };
        let sink = Sink {
            ring: consumer,
            shared,
            channels: format.channels,
            offset: 0,
            counted: 0,
            frames: 0,
        };
        Ok((engine, sink))
    }

    /// Replaces whatever plays with `source`, from where it stands, unpaused.
    pub(super) fn play(&mut self, source: Source) -> Result<()> {
        self.next_generation();
        self.offset = source.start();
        self.cancel = source.cancel_handle();
        self.shared.paused.store(false, Ordering::Release);
        self.jobs
            .send(Job::Play(self.generation, source))
            .map_err(|_| anyhow!("the decoder thread stopped"))
    }

    /// Silence until the next [`play`](Self::play).
    pub(super) fn stop(&mut self) {
        self.next_generation();
        self.offset = Duration::ZERO;
        // A decoder thread that is gone has nothing left to stop.
        let _ = self.jobs.send(Job::Stop);
    }

    /// The old generation must be out of date before `paused` lets `fill`
    /// play again, or the old track sounds for one callback.
    fn next_generation(&mut self) {
        self.generation += 1;
        self.shared
            .generation
            .store(self.generation, Ordering::Release);
        self.end_stream();
    }

    fn end_stream(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
    }

    pub(super) fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Release);
    }

    pub(super) fn set_volume(&self, volume: f32) {
        self.shared
            .volume
            .store(volume.to_bits(), Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn volume(&self) -> f32 {
        f32::from_bits(self.shared.volume.load(Ordering::Acquire))
    }

    /// True once the current track has been played to its end.
    pub(super) fn finished(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire) == self.generation
    }

    /// The place in the current track: where it began, plus what the sound
    /// card played of it.
    pub(super) fn position(&self) -> Duration {
        let progress = self.shared.progress.load(Ordering::Acquire);
        if (progress >> 32) as u32 != self.generation {
            return self.offset;
        }
        let frames = progress & u64::from(u32::MAX);
        self.offset + Duration::from_secs_f64(frames as f64 / f64::from(self.rate))
    }

    /// Fails once the sound card's stream or the decoder thread is gone.
    pub(super) fn check(&self) -> Result<()> {
        let error = self
            .shared
            .error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(error) = error {
            bail!("the sound output failed: {error}");
        }
        ensure!(!self.decoder.is_finished(), "the decoder thread stopped");
        Ok(())
    }

    pub(super) fn failures(&self) -> Failures {
        Failures(Arc::clone(&self.shared))
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.end_stream();
    }
}

impl Sink {
    /// Fills `out` (whole frames, interleaved) with the current track, or
    /// silence. This runs on the sound card's real-time thread: it takes no
    /// lock, allocates nothing and never waits.
    pub(super) fn fill<T: Sample + FromSample<f32>>(&mut self, out: &mut [T]) {
        let generation = self.shared.generation.load(Ordering::Acquire);
        // Done before the pause check, so a stopped track leaves the ring
        // even while paused, and the decoder is never stuck on a full ring.
        while self
            .ring
            .peek()
            .is_ok_and(|chunk| chunk.generation < generation)
        {
            self.discard();
        }
        let mut written = 0;
        if !self.shared.paused.load(Ordering::Acquire) {
            let volume = f32::from_bits(self.shared.volume.load(Ordering::Acquire));
            while written < out.len() {
                let Ok(chunk) = self.ring.peek() else { break };
                // A newer track than the one loaded above: the next call plays it.
                if chunk.generation != generation {
                    break;
                }
                if chunk.end {
                    self.shared.finished.store(generation, Ordering::Release);
                    self.discard();
                    continue;
                }
                let len = chunk.len;
                let n = (len - self.offset).min(out.len() - written);
                let samples = &chunk.samples[self.offset..self.offset + n];
                for (out, sample) in out[written..written + n].iter_mut().zip(samples) {
                    *out = T::from_sample(sample * volume);
                }
                written += n;
                self.offset += n;
                if self.offset == len {
                    self.discard();
                }
            }
            self.count(generation, written);
        }
        out[written..].fill(T::EQUILIBRIUM);
    }

    fn discard(&mut self) {
        if let Ok(chunk) = self.ring.read_chunk(1) {
            chunk.commit_all();
        }
        self.offset = 0;
    }

    fn count(&mut self, generation: u32, samples: usize) {
        if generation != self.counted {
            self.counted = generation;
            self.frames = 0;
        }
        self.frames += (samples / self.channels) as u64;
        let frames = self.frames.min(u64::from(u32::MAX));
        self.shared
            .progress
            .store((u64::from(generation) << 32) | frames, Ordering::Release);
    }
}

/// The decoder thread: decodes the latest job's track into the ring until the
/// track ends, a newer job arrives, or the [`Engine`] or [`Sink`] is dropped.
fn decode_jobs(jobs: &Receiver<Job>, mut ring: Producer<Chunk>, format: Format, chunk_len: usize) {
    let mut feed: Option<Feed> = None;
    loop {
        let job = match &feed {
            None => match jobs.recv() {
                Ok(job) => Some(job),
                Err(_) => return,
            },
            Some(_) if ring.is_full() => match jobs.recv_timeout(FULL_WAIT) {
                Ok(job) => Some(job),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            Some(_) => match jobs.try_recv() {
                Ok(job) => Some(job),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            },
        };
        if let Some(job) = job {
            feed = match job {
                Job::Play(generation, source) => {
                    Some(Feed::new(generation, source, format, chunk_len))
                }
                Job::Stop => None,
            };
            continue;
        }
        if ring.is_abandoned() {
            return;
        }
        if ring.is_full() {
            continue;
        }
        if let Some(current) = &mut feed {
            match current.next_chunk() {
                // Cannot fail: this thread is the only producer and saw room.
                Some(chunk) => {
                    let _ = ring.push(chunk);
                }
                None => feed = None,
            }
        }
    }
}

/// One track on its way into the ring.
struct Feed {
    generation: u32,
    source: Source,
    converter: Converter,
    /// The last decoded packet, as the file has it.
    decoded: Vec<f32>,
    /// Converted samples not in a chunk yet; `taken` of them are.
    converted: Vec<f32>,
    taken: usize,
    chunk_len: usize,
    decoded_all: bool,
    sent_end: bool,
}

impl Feed {
    fn new(generation: u32, source: Source, format: Format, chunk_len: usize) -> Feed {
        Feed {
            generation,
            source,
            converter: Converter::new(format.rate, format.channels),
            decoded: Vec::new(),
            converted: Vec::new(),
            taken: 0,
            chunk_len,
            decoded_all: false,
            sent_end: false,
        }
    }

    /// The track's next chunk, then its end marker, then `None`.
    fn next_chunk(&mut self) -> Option<Chunk> {
        while self.converted.len() - self.taken < self.chunk_len && !self.decoded_all {
            self.converted.drain(..self.taken);
            self.taken = 0;
            match self.source.next(&mut self.decoded) {
                Some((rate, channels)) => {
                    self.converter
                        .push(&self.decoded, channels, rate, &mut self.converted);
                }
                None => self.decoded_all = true,
            }
        }
        let n = (self.converted.len() - self.taken).min(self.chunk_len);
        if n > 0 {
            let chunk = Chunk::audio(self.generation, &self.converted[self.taken..self.taken + n]);
            self.taken += n;
            return Some(chunk);
        }
        if self.sent_end {
            return None;
        }
        self.sent_end = true;
        Some(Chunk::end(self.generation))
    }
}
