//! Radio: the network reader, and stations on the fake sound card.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::netread::{Limits, NetRead};
use super::*;
use crate::player::tests::{head, station, web};

/// Each connection gets `body`, then the server closes it.
fn closing(body: &'static [u8], content_type: &'static str) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&hits);
    let base = web(move |_, stream| {
        counted.fetch_add(1, Ordering::SeqCst);
        head(stream, 200, content_type);
        let _ = stream.write_all(body);
    });
    (format!("{base}/live"), hits)
}

/// Each connection gets `body`, then nothing more for a minute.
fn stalling(body: &'static [u8]) -> String {
    let base = web(move |_, stream: &mut TcpStream| {
        head(stream, 200, "audio/mpeg");
        let _ = stream.write_all(body);
        let _ = stream.flush();
        thread::sleep(Duration::from_secs(60));
    });
    format!("{base}/live")
}

fn limits(idle_ms: u64, reconnects: u32) -> Limits {
    Limits {
        idle: Duration::from_millis(idle_ms),
        reconnects,
    }
}

#[test]
fn a_stream_that_closes_is_fetched_again_three_times() {
    let (url, hits) = closing(b"abc", "audio/mpeg");
    let mut stream = NetRead::open(&url, limits(5000, 3)).unwrap();
    let mut heard = Vec::new();
    let err = stream.read_to_end(&mut heard).unwrap_err();
    assert_eq!(heard, b"abcabcabcabc");
    assert!(err.to_string().contains("3 new connections"), "{err}");
    assert_eq!(hits.load(Ordering::SeqCst), 4);
}

#[test]
fn a_stream_that_sends_nothing_is_fetched_again() {
    let url = stalling(b"abc");
    let mut stream = NetRead::open(&url, limits(100, 1)).unwrap();
    let mut heard = Vec::new();
    let err = stream.read_to_end(&mut heard).unwrap_err();
    assert_eq!(heard, b"abcabc");
    assert!(err.to_string().contains("sent nothing"), "{err}");
}

#[test]
fn a_stream_that_is_not_there_fails_at_once() {
    let gone = web(|_, stream| head(stream, 404, "text/html"));
    let err = NetRead::open(&format!("{gone}/live"), Limits::default())
        .err()
        .unwrap();
    assert!(format!("{err:#}").contains("404"), "{err:#}");
}

#[test]
fn cancel_ends_a_read_that_waits() {
    let url = stalling(b"");
    let mut stream = NetRead::open(&url, Limits::default()).unwrap();
    let cancel = stream.cancel_handle();
    let reader = thread::spawn(move || {
        let started = Instant::now();
        let read = stream.read(&mut [0; 16]).unwrap();
        (read, started.elapsed())
    });
    thread::sleep(Duration::from_millis(50));
    cancel.cancel();
    let (read, waited) = reader.join().unwrap();
    assert_eq!(read, 0, "end of stream");
    assert!(waited < Duration::from_secs(2), "{waited:?}");
}

/// `tone.mp3` as a live stream sends it: MPEG frames only. The file's ID3
/// tag and its first frame, a LAME "Info" frame with the frame count, go:
/// symphonia stops after that many frames.
fn tone() -> &'static [u8] {
    let file = std::fs::read(fixture("tone.mp3")).unwrap();
    assert_eq!(&file[..3], b"ID3");
    let tag = 10
        + file[6..10]
            .iter()
            .fold(0, |size, byte| size << 7 | usize::from(*byte));
    let info = &file[tag..];
    assert!(info.windows(4).take(64).any(|w| w == b"Info"));
    // MPEG-1 Layer III: 144 × bit rate / sample rate, plus the padding byte.
    let kbps = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    let rates = [44_100, 48_000, 32_000];
    let bits = info[2];
    let length = 144_000 * kbps[usize::from(bits >> 4)] / rates[usize::from(bits >> 2 & 3)]
        + usize::from(bits >> 1 & 1);
    Box::leak(info[length..].to_vec().into_boxed_slice())
}

#[test]
fn a_stream_decodes_on_across_new_connections_and_has_no_length() {
    let dir = TempDir::new().unwrap();
    let once = dir.path().join("once.mp3");
    std::fs::write(&once, tone()).unwrap();
    let one = decode_all(&once).2.len();

    let (url, _) = closing(tone(), "audio/mpeg");
    let stream = NetRead::open(&url, limits(5000, 3)).unwrap();
    let mut source = Source::open_stream(stream, Some("audio/mpeg")).unwrap();
    assert_eq!(source.duration(), None);
    let (mut all, mut packet) = (0, Vec::new());
    while source.next(&mut packet).is_some() {
        all += packet.len();
    }
    // The first connection and three more; the decoder may drop a frame
    // where one stream meets the next.
    assert!(
        all.abs_diff(4 * one) <= 4 * 1152,
        "{all} samples, {one} each"
    );
}

fn play_station(rig: &mut Rig, url: String) -> Result<()> {
    rig.send(PlayerCmd::Play {
        item: ITEM,
        content: station(url),
        volume: 1.0,
    })
}

#[test]
fn a_station_plays_until_its_stream_stays_gone() {
    let mut rig = Rig::new();
    let (url, hits) = closing(tone(), "audio/mpeg");
    play_station(&mut rig, url).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Playing(ITEM)]);
    assert_eq!(rig.player.poll_interval(), Some(POLL_INTERVAL));

    let heard = rig.finish_track();
    // The 0.55 s tone, once per connection: the first and three more.
    let secs = heard.iter().filter(|s| **s != 0.0).count() as f64 / 2.0 / f64::from(RATE);
    assert!((1.8..2.6).contains(&secs), "{secs} s");
    assert!(peak(&heard) > 0.1);
    assert_eq!(rig.events(), [PlayerEvent::Stopped]);
    assert_eq!(rig.player.poll_interval(), None);
    // One for resolving the station, four for playing it.
    assert_eq!(hits.load(Ordering::SeqCst), 5);
}

#[test]
fn next_and_prev_do_nothing_on_a_station() {
    let mut rig = Rig::new();
    let (url, _) = closing(tone(), "audio/mpeg");
    play_station(&mut rig, url).unwrap();
    rig.events();
    rig.send(PlayerCmd::Next).unwrap();
    rig.send(PlayerCmd::Prev).unwrap();
    assert_eq!(rig.events(), []);
    assert_eq!(rig.player.current, Some(0));
    assert!(peak(&rig.listen(0.2)) > 0.1);
}

#[test]
fn pause_silences_the_station_and_play_fetches_it_again() {
    let mut rig = Rig::new();
    let (url, hits) = closing(tone(), "audio/mpeg");
    play_station(&mut rig, url).unwrap();
    rig.listen(0.1);
    rig.events();

    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Paused(ITEM)]);
    for _ in 0..5 {
        assert!(levels(&rig.fill(), 1.0).is_empty());
    }
    rig.poll().unwrap();
    assert_eq!(rig.events(), [], "a paused station has not ended");

    let before = hits.load(Ordering::SeqCst);
    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(rig.events(), [PlayerEvent::Playing(ITEM)]);
    assert!(hits.load(Ordering::SeqCst) > before);
    assert!(peak(&rig.listen(0.2)) > 0.1);
}

#[test]
fn he_aac_and_hls_stations_do_not_open_the_sound_card() {
    let mut rig = Rig::new();
    let (aacp, _) = closing(b"not decoded", "audio/aacp");
    let (hls, _) = closing(
        b"#EXTM3U\n#EXT-X-VERSION:3\nchunk.m3u8\n",
        "application/vnd.apple.mpegurl",
    );
    for (url, kind) in [(aacp, "HE-AAC"), (hls, "HLS")] {
        let err = play_station(&mut rig, url).unwrap_err();
        assert!(format!("{err:#}").contains(kind), "{err:#}");
    }
    assert_eq!(rig.opened.get(), 0);
    assert_eq!(rig.events(), []);
}

#[test]
fn a_station_that_is_not_audio_does_not_open_the_sound_card() {
    let mut rig = Rig::new();
    let (url, _) = closing(&[0; 4096], "audio/mpeg");
    assert!(play_station(&mut rig, url).is_err());
    assert_eq!(rig.opened.get(), 0);
}

#[test]
fn an_album_after_a_station_plays_its_tracks() {
    let mut rig = Rig::new();
    let (url, _) = closing(tone(), "audio/mpeg");
    play_station(&mut rig, url).unwrap();
    let tracks = vec![rig.track("1.wav", 0.2, 1000), rig.track("2.wav", 0.2, 2000)];
    rig.play_album(tracks, 1.0).unwrap();
    assert_eq!(levels(&rig.finish_track(), 1.0), [1000]);
    assert_eq!(rig.player.current, Some(1));
}

#[test]
#[ignore = "needs the internet: cargo test -- --ignored"]
fn decodes_real_stations() {
    crate::net::install_crypto();
    for url in [
        "http://streamtdy.ir-media-tec.com/kinderlieder/mp3-128/web/play.mp3",
        "https://stream.rpr1.de/kidshits/mp3-128/radiobrowser",
    ] {
        let stream = crate::radio::resolve(&crate::net::stream_agent(), url).unwrap();
        let reader = NetRead::open(&stream.url, Limits::default()).unwrap();
        let mut source = Source::open_stream(reader, stream.content_type.as_deref()).unwrap();
        let (mut samples, mut packet, mut format) = (Vec::new(), Vec::new(), (0, 0));
        while samples.len() < 44_100 * 2 * 3 {
            format = source.next(&mut packet).expect("the stream ended");
            samples.extend_from_slice(&packet);
        }
        println!("{url}: {stream:?}, {format:?}, peak {}", peak(&samples));
        assert!(peak(&samples) > 0.01, "{url} is silent");
    }
}

#[test]
fn stop_closes_the_sound_card_until_the_next_item() {
    let mut rig = Rig::new();
    let (url, _) = closing(tone(), "audio/mpeg");
    play_station(&mut rig, url).unwrap();
    rig.player.halt(&mut rig.emitter);
    assert!(rig.player.output.is_none());
    assert_eq!(rig.player.poll_interval(), None);
    assert_eq!(
        rig.events(),
        [PlayerEvent::Playing(ITEM)],
        "the caller reports what follows"
    );

    let tracks = vec![rig.track("1.wav", 0.2, 1000)];
    rig.play_album(tracks, 1.0).unwrap();
    assert_eq!(rig.opened.get(), 2);
    assert_eq!(levels(&rig.listen(0.1), 1.0), [1000]);
}

#[test]
fn stop_reports_where_a_book_got_to() {
    let mut rig = Rig::new();
    rig.emitter.progress.interval = Duration::ZERO;
    let tracks = vec![rig.track("1.wav", 1.0, 1000)];
    rig.send(PlayerCmd::Play {
        item: ITEM,
        content: Content::Tracks {
            tracks,
            start: Start::default(),
            progress: true,
        },
        volume: 1.0,
    })
    .unwrap();
    rig.listen(0.3);
    rig.events();
    rig.player.halt(&mut rig.emitter);
    let reported = rig.events().iter().any(|event| {
        matches!(event, PlayerEvent::Progress { position, .. } if *position >= Duration::from_millis(300))
    });
    assert!(reported);
}
