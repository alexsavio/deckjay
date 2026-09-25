use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Instant;

use anyhow::ensure;
use tempfile::TempDir;

use super::decode::Converter;
use super::engine::{Engine, Format, Sink};
use super::output::pick;
use super::*;
use crate::player::{Content, Start};

/// The fake sound card's rate, also the rate of the test tracks, so their
/// samples reach the card unchanged.
const RATE: u32 = 8000;
const ITEM: ItemId = ItemId(4);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/player/local/fixtures")
        .join(name)
}

/// Writes a 16-bit PCM WAV file.
fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[i16]) {
    let data_len = (samples.len() * 2) as u32;
    let block_align = channels * 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * u32::from(block_align)).to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

/// The whole file: its rate, its channel count and all its samples.
fn decode_all(path: &Path) -> (u32, usize, Vec<f32>) {
    let mut source = Source::open(path).unwrap();
    let (mut all, mut packet, mut format) = (Vec::new(), Vec::new(), (0, 0));
    while let Some(packet_format) = source.next(&mut packet) {
        format = packet_format;
        all.extend_from_slice(&packet);
    }
    (format.0, format.1, all)
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |max, s| max.max(s.abs()))
}

#[test]
fn decodes_a_wav_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("stereo.wav");
    let samples: Vec<i16> = (0..800).map(|i| (i * 40 - 16_000) as i16).collect();
    write_wav(&path, RATE, 2, &samples);

    let (rate, channels, decoded) = decode_all(&path);
    assert_eq!((rate, channels), (RATE, 2));
    assert_eq!(decoded.len(), samples.len());
    for (decoded, sample) in decoded.iter().zip(&samples) {
        assert!((decoded - f32::from(*sample) / 32_768.0).abs() < 1e-6);
    }
}

#[test]
fn decodes_mp3_and_aac() {
    for name in ["tone.mp3", "tone.m4a"] {
        let (rate, channels, samples) = decode_all(&fixture(name));
        assert_eq!((rate, channels), (44_100, 1), "{name}");
        // 0.5 s, give or take the encoder's padding.
        let secs = samples.len() as f64 / 44_100.0;
        assert!((0.45..0.6).contains(&secs), "{name}: {secs} s");
        assert!(peak(&samples) > 0.1, "{name} is silent");
    }
}

#[test]
fn refuses_files_it_cannot_decode() {
    let dir = TempDir::new().unwrap();
    let junk = dir.path().join("song.mp3");
    std::fs::write(&junk, "not music at all\n".repeat(100)).unwrap();
    for path in [junk, dir.path().join("missing.wav"), fixture("tone.opus")] {
        assert!(Source::open(&path).is_err(), "{}", path.display());
    }
}

#[test]
fn resamples_mono_to_stereo_at_the_output_rate() {
    // 0.5 s of 440 Hz at 44.1 kHz.
    let input: Vec<f32> = (0..22_050).map(|i| (i as f32 * 0.0627).sin()).collect();
    let mut out = Vec::new();
    let mut converter = Converter::new(48_000, 2);
    // In two packets: the resampler must carry its position across them.
    let (first, second) = input.split_at(10_001);
    converter.push(first, 1, 44_100, &mut out);
    converter.push(second, 1, 44_100, &mut out);

    assert_eq!(out.len() % 2, 0);
    let frames = out.len() / 2;
    assert!(frames.abs_diff(24_000) <= 2, "{frames} frames");
    let (frame_pairs, _) = out.as_chunks::<2>();
    assert!(frame_pairs.iter().all(|[l, r]| l.to_bits() == r.to_bits()));
    assert!(peak(&out) > 0.99);

    let mut at_once = Vec::new();
    Converter::new(48_000, 2).push(&input, 1, 44_100, &mut at_once);
    assert_eq!(out, at_once);
}

#[test]
fn maps_channels_to_the_output() {
    let convert = |input: &[f32], in_channels, out_channels| {
        let mut out = Vec::new();
        Converter::new(RATE, out_channels).push(input, in_channels, RATE, &mut out);
        out
    };
    let stereo = [0.25, 0.75, -0.5, 0.0];
    assert_eq!(convert(&stereo, 2, 1), [0.5, -0.25]);
    assert_eq!(
        convert(&stereo, 2, 4),
        [0.25, 0.75, 0.0, 0.0, -0.5, 0.0, 0.0, 0.0]
    );
    assert_eq!(convert(&[0.5, -0.5], 1, 2), [0.5, 0.5, -0.5, -0.5]);
    // 5.1: the front left and right.
    assert_eq!(convert(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6], 6, 2), [0.1, 0.2]);
}

#[test]
fn an_output_is_picked_by_part_of_its_name() {
    let names = [
        "Default Audio Device (default)",
        "bcm2835 Headphones, bcm2835 Headphones (hw:CARD=Headphones,DEV=0)",
        "vc4-hdmi, MAI PCM i2s-hifi-0 (hdmi:CARD=vc4hdmi,DEV=0)",
    ]
    .map(String::from);
    assert_eq!(pick(&names, "headphones").unwrap(), 1);
    assert_eq!(pick(&names, "HDMI:").unwrap(), 2);

    let err = pick(&names, "USB").unwrap_err().to_string();
    for name in &names {
        assert!(err.contains(name.as_str()), "{err}");
    }
    assert!(pick(&[], "USB").is_err());
}

#[test]
fn listing_the_outputs_does_not_panic() {
    // Machines without a sound card (CI) get an error or an empty list.
    let _ = output_devices();
}

/// The player with a fake sound card: the test plays the sink itself.
struct Rig {
    player: LocalPlayer,
    emitter: Emitter,
    events: Receiver<PlayerEvent>,
    /// The sink of the output opened last.
    sink: Rc<RefCell<Option<Sink>>>,
    opened: Rc<Cell<usize>>,
    no_sound_card: Rc<Cell<bool>>,
    dir: TempDir,
}

impl Rig {
    fn new() -> Rig {
        let sink: Rc<RefCell<Option<Sink>>> = Rc::default();
        let opened: Rc<Cell<usize>> = Rc::default();
        let no_sound_card: Rc<Cell<bool>> = Rc::default();
        let open: Open = {
            let (sink, opened, no_sound_card) =
                (sink.clone(), opened.clone(), no_sound_card.clone());
            Box::new(move || {
                opened.set(opened.get() + 1);
                ensure!(!no_sound_card.get(), "no sound card");
                let format = Format {
                    rate: RATE,
                    channels: 2,
                };
                let (engine, new_sink) = Engine::new(format)?;
                *sink.borrow_mut() = Some(new_sink);
                Ok(OpenOutput::without_device(engine))
            })
        };
        let (tx, events) = mpsc::channel();
        Rig {
            player: LocalPlayer::with_output(open),
            emitter: Emitter { tx, last: None },
            events,
            sink,
            opened,
            no_sound_card,
            dir: TempDir::new().unwrap(),
        }
    }

    /// A mono track that holds one `level` throughout, so what plays tells
    /// which track it is.
    fn track(&self, name: &str, secs: f64, level: i16) -> TrackInfo {
        let path = self.dir.path().join(name);
        write_wav(
            &path,
            RATE,
            1,
            &vec![level; (secs * f64::from(RATE)) as usize],
        );
        track_info(path)
    }

    fn junk(&self, name: &str) -> TrackInfo {
        let path = self.dir.path().join(name);
        std::fs::write(&path, "not music at all\n".repeat(100)).unwrap();
        track_info(path)
    }

    fn play_album(&mut self, tracks: Vec<TrackInfo>, volume: f32) -> Result<()> {
        self.send(PlayerCmd::Play {
            item: ITEM,
            content: Content::Tracks {
                tracks,
                start: Start::default(),
                progress: false,
            },
            volume,
        })
    }

    /// As `player::run` sends a command.
    fn send(&mut self, cmd: PlayerCmd) -> Result<()> {
        self.emitter.last = None;
        self.player.handle(cmd, &mut self.emitter)
    }

    fn poll(&mut self) -> Result<()> {
        self.player.poll(&mut self.emitter)
    }

    fn events(&self) -> Vec<PlayerEvent> {
        self.events.try_iter().collect()
    }

    fn engine(&self) -> &Engine {
        &self.player.output.as_ref().expect("no open output").engine
    }

    /// One 10 ms buffer of the fake sound card; waits a little on silence,
    /// which is the decoder thread still catching up.
    fn fill(&self) -> Vec<f32> {
        let mut buf = vec![0.0; RATE as usize / 100 * 2];
        self.sink
            .borrow_mut()
            .as_mut()
            .expect("no sound card open")
            .fill(&mut buf);
        if buf.iter().all(|s| *s == 0.0) {
            thread::sleep(Duration::from_millis(1));
        }
        buf
    }

    /// Plays the current track until `secs` of it are heard, or to its end;
    /// returns what the sound card got.
    fn listen(&self, secs: f64) -> Vec<f32> {
        let until = Duration::from_secs_f64(secs);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut heard = Vec::new();
        while self.engine().position() < until && !self.engine().finished() {
            assert!(Instant::now() < deadline, "the track does not play");
            heard.extend(self.fill());
        }
        heard
    }

    /// Plays the current track to its end and lets the player see it.
    fn finish_track(&mut self) -> Vec<f32> {
        let heard = self.listen(3600.0);
        self.poll().unwrap();
        heard
    }
}

fn track_info(path: PathBuf) -> TrackInfo {
    TrackInfo {
        url: String::new(),
        title: path.file_stem().unwrap().to_string_lossy().into_owned(),
        path,
        content_type: "audio/wav".into(),
        album: "Test".into(),
        cover_url: None,
    }
}

/// The track levels in `samples`, in order, without silence and repeats.
fn levels(samples: &[f32], volume: f32) -> Vec<i16> {
    let mut levels: Vec<i16> = Vec::new();
    for sample in samples.iter().filter(|s| **s != 0.0) {
        let level = (sample / volume * 32_768.0).round() as i16;
        if levels.last() != Some(&level) {
            levels.push(level);
        }
    }
    levels
}

#[test]
fn plays_the_album_track_by_track_then_stops() {
    let mut rig = Rig::new();
    assert_eq!(
        rig.opened.get(),
        0,
        "the sound card opens at the first album"
    );
    let tracks = vec![
        rig.track("1.wav", 0.2, 1000),
        rig.track("2.wav", 0.2, 2000),
        rig.track("3.wav", 0.2, 3000),
    ];
    rig.play_album(tracks, 1.0).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Playing(ITEM)]);
    assert_eq!(rig.player.poll_interval(), Some(POLL_INTERVAL));

    let heard = rig.finish_track();
    assert_eq!(levels(&heard, 1.0), [1000]);
    // Every sample once, on both channels: 0.2 s at 8 kHz.
    assert_eq!(heard.iter().filter(|s| **s != 0.0).count(), 1600 * 2);
    assert_eq!(rig.player.current, Some(1));
    assert_eq!(levels(&rig.finish_track(), 1.0), [2000]);
    assert_eq!(levels(&rig.finish_track(), 1.0), [3000]);

    assert_eq!(rig.events(), [PlayerEvent::Stopped]);
    assert_eq!(rig.player.current, None);
    assert_eq!(rig.player.poll_interval(), None);
    assert!(levels(&rig.fill(), 1.0).is_empty());
    assert_eq!(rig.opened.get(), 1);
}

#[test]
fn next_cuts_to_the_next_track_at_once_and_does_nothing_on_the_last() {
    let mut rig = Rig::new();
    let tracks = vec![rig.track("1.wav", 1.0, 1000), rig.track("2.wav", 1.0, 2000)];
    rig.play_album(tracks, 1.0).unwrap();
    rig.listen(0.1);
    rig.events();

    rig.send(PlayerCmd::Next).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Playing(ITEM)]);
    // The rest of track 1 waits in the buffer by now; none of it is heard.
    assert_eq!(levels(&rig.listen(0.3), 1.0), [2000]);

    rig.send(PlayerCmd::Next).unwrap();
    assert_eq!(rig.events(), []);
    assert_eq!(rig.player.current, Some(1));
    assert_eq!(levels(&rig.listen(0.6), 1.0), [2000]);
}

#[test]
fn prev_goes_back_early_in_a_track_and_restarts_it_later() {
    let mut rig = Rig::new();
    let tracks = vec![rig.track("1.wav", 6.0, 1000), rig.track("2.wav", 6.0, 2000)];
    rig.play_album(tracks, 1.0).unwrap();
    rig.send(PlayerCmd::Prev).unwrap();
    assert_eq!(
        rig.player.current,
        Some(0),
        "there is no track before the first"
    );

    rig.send(PlayerCmd::Next).unwrap();
    rig.listen(1.0);
    rig.send(PlayerCmd::Prev).unwrap();
    assert_eq!(rig.player.current, Some(0));
    assert_eq!(levels(&rig.listen(0.2), 1.0), [1000]);

    rig.send(PlayerCmd::Next).unwrap();
    rig.listen(5.1);
    rig.send(PlayerCmd::Prev).unwrap();
    assert_eq!(rig.player.current, Some(1));
    assert_eq!(rig.engine().position(), Duration::ZERO, "it starts again");
    assert_eq!(levels(&rig.listen(0.2), 1.0), [2000]);
}

#[test]
fn toggle_pause_pauses_resumes_and_restarts_a_finished_album() {
    let mut rig = Rig::new();
    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Stopped], "no album yet");

    let tracks = vec![rig.track("1.wav", 0.5, 1000)];
    rig.play_album(tracks, 1.0).unwrap();
    rig.listen(0.1);
    rig.events();

    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Paused(ITEM)]);
    let at = rig.engine().position();
    for _ in 0..5 {
        assert!(levels(&rig.fill(), 1.0).is_empty());
    }
    rig.poll().unwrap();
    assert_eq!(rig.engine().position(), at, "a pause keeps the place");

    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Playing(ITEM)]);
    assert_eq!(levels(&rig.finish_track(), 1.0), [1000]);
    assert_eq!(rig.events(), [PlayerEvent::Stopped]);

    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Playing(ITEM)]);
    assert_eq!(rig.player.current, Some(0));
    assert_eq!(levels(&rig.listen(0.1), 1.0), [1000]);
}

#[test]
fn volume_is_clamped_and_scales_the_sound() {
    let mut rig = Rig::new();
    let tracks = vec![rig.track("1.wav", 1.0, 1000)];
    rig.play_album(tracks, 2.0).unwrap();
    assert!((rig.engine().volume() - 1.0).abs() < f32::EPSILON);
    rig.send(PlayerCmd::SetVolume(-0.5)).unwrap();
    assert!(rig.engine().volume().abs() < f32::EPSILON);

    rig.send(PlayerCmd::SetVolume(0.5)).unwrap();
    let heard = rig.listen(0.1);
    assert_eq!(levels(&heard, 0.5), [1000]);
    assert!((peak(&heard) - 500.0 / 32_768.0).abs() < 1e-6);
}

#[test]
fn an_undecodable_track_is_skipped() {
    let mut rig = Rig::new();
    let tracks = vec![
        rig.junk("1.mp3"),
        rig.track("2.wav", 0.2, 2000),
        track_info(fixture("tone.opus")),
    ];
    rig.play_album(tracks, 1.0).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Playing(ITEM)]);
    assert_eq!(rig.player.current, Some(1));
    assert_eq!(levels(&rig.finish_track(), 1.0), [2000]);
    assert_eq!(rig.events(), [PlayerEvent::Stopped]);

    let tracks = vec![rig.junk("1.mp3")];
    rig.play_album(tracks, 1.0).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Stopped]);
}

#[test]
fn without_a_sound_card_an_album_fails_until_there_is_one() {
    let mut rig = Rig::new();
    rig.no_sound_card.set(true);
    let tracks = vec![rig.track("1.wav", 0.2, 1000)];
    assert!(rig.play_album(tracks, 1.0).is_err());
    rig.player.reset();

    rig.no_sound_card.set(false);
    let tracks = vec![rig.track("1.wav", 0.2, 1000)];
    rig.play_album(tracks, 1.0).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Playing(ITEM)]);
    assert_eq!(rig.opened.get(), 2);
}

#[test]
fn a_broken_stream_fails_the_next_command_and_poll_and_an_album_reopens_it() {
    let mut rig = Rig::new();
    let tracks = vec![rig.track("1.wav", 1.0, 1000), rig.track("2.wav", 1.0, 2000)];
    rig.play_album(tracks, 1.0).unwrap();
    rig.engine()
        .failures()
        .report("the device was unplugged".into());
    assert!(rig.poll().is_err());
    let err = rig.send(PlayerCmd::Next).unwrap_err();
    assert!(format!("{err:#}").contains("unplugged"), "{err:#}");
    for cmd in [
        PlayerCmd::TogglePause,
        PlayerCmd::Prev,
        PlayerCmd::SetVolume(0.5),
    ] {
        assert!(rig.send(cmd).is_err());
    }
    rig.player.reset();
    assert_eq!(rig.player.poll_interval(), None);

    let tracks = vec![rig.track("1.wav", 1.0, 1000)];
    rig.play_album(tracks, 1.0).unwrap();
    assert_eq!(rig.opened.get(), 2);
    assert_eq!(levels(&rig.listen(0.1), 1.0), [1000]);

    // An album pressed right after the failure opens the output again too.
    rig.engine().failures().report("gone again".into());
    let tracks = vec![rig.track("1.wav", 1.0, 1000)];
    rig.play_album(tracks, 1.0).unwrap();
    assert_eq!(rig.opened.get(), 3);
    assert_eq!(levels(&rig.listen(0.1), 1.0), [1000]);
}

#[test]
fn radio_and_spotify_do_not_open_the_sound_card() {
    let mut rig = Rig::new();
    for content in crate::player::tests::unsupported() {
        let cmd = PlayerCmd::Play {
            item: ITEM,
            content,
            volume: 1.0,
        };
        let err = rig.send(cmd).unwrap_err();
        assert!(format!("{err:#}").contains("not supported yet"), "{err:#}");
    }
    assert_eq!(rig.opened.get(), 0);
    assert_eq!(rig.events(), []);
}
