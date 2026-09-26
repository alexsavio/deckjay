use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

#[test]
fn every_failed_command_reports_stopped() {
    // Nothing listens on this port, so every command fails at once.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let host = String::from("127.0.0.1");
    for output in [
        Output::Cast {
            host: host.clone(),
            port,
        },
        Output::Heos { host, port },
    ] {
        let (tx, events) = mpsc::channel();
        let player = spawn(output.clone(), tx);

        for _ in 0..2 {
            player.send(PlayerCmd::TogglePause).unwrap();
            assert_eq!(
                events.recv_timeout(Duration::from_secs(5)),
                Ok(PlayerEvent::Stopped),
                "{output:?}"
            );
        }
    }
}

#[test]
fn connect_tries_every_address() {
    // `localhost` resolves to ::1 first on macOS, and only IPv4 listens here.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    connect("localhost", port).unwrap();
}

#[test]
fn volumes_are_clamped_to_the_speaker_range() {
    for (volume, expected) in [(-0.1, 0.0), (0.35, 0.35), (1.5, 1.0)] {
        assert!(
            (clamp_volume(volume) - expected).abs() < f32::EPSILON,
            "{volume}"
        );
    }
}

fn album() -> PlayerCmd {
    PlayerCmd::Play {
        item: ItemId(0),
        content: Content::Tracks {
            tracks: vec![],
            start: Start::default(),
            progress: false,
        },
        volume: 0.2,
    }
}

/// Content no backend plays yet.
pub(super) fn unsupported() -> Content {
    Content::Spotify(Playlist {
        uri: "spotify:playlist:37i9dQZF1DX0XUsuxWHRQd".into(),
        name: "Bedtime".into(),
    })
}

/// A station whose key points at `url`.
pub(super) fn station(url: String) -> Content {
    Content::Stream(Station {
        url,
        content_type: None,
        name: "Kids Radio".into(),
        cover_url: None,
    })
}

/// A web server on 127.0.0.1, one thread per connection: `answer` gets the
/// path of each GET and writes the whole reply. Returns the server's URL.
pub(super) fn web(answer: impl Fn(&str, &mut TcpStream) + Send + Sync + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let answer = Arc::new(answer);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let answer = Arc::clone(&answer);
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                let _ = reader.read_line(&mut request);
                // The rest of the request, so closing sends no reset.
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                answer(path, &mut stream);
            });
        }
    });
    base
}

/// The head of a reply as radio servers send it: no length, the body lasts
/// until the connection closes.
pub(super) fn head(stream: &mut TcpStream, status: u16, content_type: &str) {
    let head =
        format!("HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n");
    let _ = stream.write_all(head.as_bytes());
}

#[test]
fn only_tracks_are_track_lists() {
    let tracks = Content::Tracks {
        tracks: vec![],
        start: Start::default(),
        progress: false,
    };
    assert!(tracks.into_tracks().is_ok());
    let err = unsupported().into_tracks().unwrap_err();
    assert!(err.to_string().contains("not supported yet"), "{err:#}");
    let err = station("http://127.0.0.1:9/kids.mp3".into())
        .into_tracks()
        .unwrap_err();
    assert!(err.to_string().contains("Kids Radio"), "{err:#}");
}

#[test]
fn a_start_track_out_of_range_is_the_first_from_its_beginning() {
    let start = |track| {
        let content = Content::Tracks {
            tracks: (0..2).map(|_| track_info()).collect(),
            start: Start {
                track,
                position: Duration::from_secs(30),
            },
            progress: true,
        };
        content.into_tracks().unwrap().start
    };
    assert_eq!(
        start(1),
        Start {
            track: 1,
            position: Duration::from_secs(30)
        }
    );
    assert_eq!(start(2), Start::default());
}

fn track_info() -> TrackInfo {
    TrackInfo {
        url: "http://10.0.0.2:8765/music/Book/01.mp3".into(),
        path: "Book/01.mp3".into(),
        content_type: "audio/mpeg".into(),
        title: "Chapter".into(),
        album: "Book".into(),
        cover_url: None,
    }
}

const BOOK: ItemId = ItemId(1);

fn place(track: usize, secs: u64) -> Place {
    Place::start_of(track, Duration::from_secs(secs))
}

fn report(track: usize, secs: u64) -> PlayerEvent {
    PlayerEvent::Progress {
        item: BOOK,
        track,
        position: Duration::from_secs(secs),
        duration: None,
    }
}

#[test]
fn pause_and_stop_report_the_latest_place_first() {
    let (tx, rx) = mpsc::channel();
    let mut events = Emitter::new(tx);
    events.begin(BOOK, true);
    events.place(place(0, 10), true);
    events.place(place(0, 12), false);
    events.emit(PlayerEvent::Paused(BOOK));
    events.emit(PlayerEvent::Playing(BOOK));
    events.place(place(0, 13), false);
    events.emit(PlayerEvent::Stopped);
    assert_eq!(
        rx.try_iter().collect::<Vec<_>>(),
        [
            report(0, 10),
            report(0, 12),
            PlayerEvent::Paused(BOOK),
            PlayerEvent::Playing(BOOK),
            report(0, 13),
            PlayerEvent::Stopped,
        ]
    );
}

#[test]
fn finished_comes_before_stopped_and_leaves_no_place() {
    let (tx, rx) = mpsc::channel();
    let mut events = Emitter::new(tx);
    events.begin(BOOK, true);
    events.place(place(2, 590), true);
    events.place(place(2, 593), false);
    events.finished();
    events.emit(PlayerEvent::Stopped);
    assert_eq!(
        rx.try_iter().collect::<Vec<_>>(),
        [
            report(2, 590),
            PlayerEvent::Finished(BOOK),
            PlayerEvent::Stopped
        ]
    );
}

#[test]
fn an_item_without_progress_gets_no_place_and_no_end() {
    let (tx, rx) = mpsc::channel();
    let mut events = Emitter::new(tx);
    events.begin(BOOK, false);
    events.place(place(0, 0), true);
    events.place(place(1, 0), true);
    events.emit(PlayerEvent::Paused(BOOK));
    events.finished();
    events.emit(PlayerEvent::Stopped);
    assert_eq!(events.resume_point(), Start::default());
    assert_eq!(
        rx.try_iter().collect::<Vec<_>>(),
        [PlayerEvent::Paused(BOOK), PlayerEvent::Stopped]
    );
}

/// Starts a book at its first track, 7 s in, and fails every other command.
struct Bookmarked;

impl Speaker for Bookmarked {
    fn handle(&mut self, cmd: PlayerCmd, events: &mut Emitter) -> Result<()> {
        let PlayerCmd::Play { item, .. } = cmd else {
            anyhow::bail!("the speaker is off");
        };
        events.begin(item, true);
        events.place(place(0, 0), true);
        events.place(place(0, 7), false);
        events.emit(PlayerEvent::Playing(item));
        Ok(())
    }

    fn poll(&mut self, _: &mut Emitter) -> Result<()> {
        Ok(())
    }

    fn poll_interval(&self) -> Option<Duration> {
        None
    }

    fn reset(&mut self) {}
}

#[test]
fn a_failed_command_reports_the_latest_place_before_stopped() {
    let (cmds, rx) = mpsc::channel();
    let book = PlayerCmd::Play {
        item: BOOK,
        content: Content::Tracks {
            tracks: vec![],
            start: Start::default(),
            progress: true,
        },
        volume: 0.2,
    };
    cmds.send(book).unwrap();
    cmds.send(PlayerCmd::TogglePause).unwrap();
    drop(cmds);
    let (tx, events) = mpsc::channel();
    run(Box::new(Bookmarked), &rx, Emitter::new(tx));
    assert_eq!(
        events.try_iter().collect::<Vec<_>>(),
        [
            report(0, 0),
            PlayerEvent::Playing(BOOK),
            report(0, 7),
            PlayerEvent::Stopped
        ]
    );
}

#[test]
fn unsupported_content_and_broken_stations_report_stopped() {
    let gone = web(|_, stream| head(stream, 404, "text/html"));
    // Nothing listens on this port: the speaker, if reached, fails too.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let host = String::from("127.0.0.1");
    for output in [
        // A local player opens the sound card only once a station answers,
        // so this runs without one.
        Output::Local { device: None },
        Output::Cast {
            host: host.clone(),
            port,
        },
        Output::Heos { host, port },
    ] {
        let (tx, events) = mpsc::channel();
        let player = spawn(output.clone(), tx);
        for content in [unsupported(), station(format!("{gone}/kids.mp3"))] {
            let cmd = PlayerCmd::Play {
                item: ItemId(1),
                content,
                volume: 0.2,
            };
            player.send(cmd).unwrap();
            assert_eq!(
                events.recv_timeout(Duration::from_secs(5)),
                Ok(PlayerEvent::Stopped),
                "{output:?}"
            );
        }
    }
}

#[test]
fn keeps_the_last_album_and_only_the_last_volume_after_it() {
    use PlayerCmd::{Next, Play, SetVolume, TogglePause};
    let out = coalesce(vec![
        SetVolume(0.1),
        album(),
        Next,
        SetVolume(0.2),
        album(),
        Next,
    ]);
    assert!(matches!(out.as_slice(), [Play { .. }, Next]), "{out:?}");

    let out = coalesce(vec![album(), SetVolume(0.3), SetVolume(0.4)]);
    assert!(
        matches!(out.as_slice(), [Play { .. }, SetVolume(v)] if (v - 0.4).abs() < f32::EPSILON),
        "{out:?}"
    );

    let out = coalesce(vec![SetVolume(0.1), TogglePause, SetVolume(0.2)]);
    assert!(
        matches!(out.as_slice(), [TogglePause, SetVolume(v)] if (v - 0.2).abs() < f32::EPSILON),
        "{out:?}"
    );
}

#[test]
fn adds_up_seeks_in_a_row_only() {
    use PlayerCmd::{Play, Seek, TogglePause};
    let out = coalesce(vec![Seek(-10), Seek(-10), Seek(10), TogglePause, Seek(10)]);
    assert!(
        matches!(out.as_slice(), [Seek(-10), TogglePause, Seek(10)]),
        "{out:?}"
    );

    let out = coalesce(vec![Seek(10), album(), Seek(10), Seek(10)]);
    assert!(matches!(out.as_slice(), [Play { .. }, Seek(20)]), "{out:?}");

    let out = coalesce(vec![Seek(10), Seek(-10), TogglePause]);
    assert!(
        matches!(out.as_slice(), [TogglePause]),
        "back and on again is no jump: {out:?}"
    );
}

enum Poll {
    Answer,
    Fail,
    /// Fails, then has no album left to poll, so `run` waits for a command.
    FailAndIdle,
}

/// A speaker that follows a script, to drive `run` without a network.
struct Scripted {
    polls: VecDeque<Poll>,
    active: bool,
    fail_commands: bool,
    handled: Arc<AtomicUsize>,
    polled: Arc<AtomicUsize>,
    /// Gets a message each time the script goes idle.
    idle: Sender<()>,
}

impl Scripted {
    fn new(polls: Vec<Poll>) -> (Scripted, Receiver<()>) {
        let (idle, idle_rx) = mpsc::channel();
        let speaker = Scripted {
            polls: polls.into(),
            active: true,
            fail_commands: false,
            handled: Arc::default(),
            polled: Arc::default(),
            idle,
        };
        (speaker, idle_rx)
    }
}

impl Speaker for Scripted {
    fn handle(&mut self, _: PlayerCmd, _: &mut Emitter) -> Result<()> {
        self.handled.fetch_add(1, Ordering::SeqCst);
        anyhow::ensure!(!self.fail_commands, "the speaker is off");
        self.active = true;
        Ok(())
    }

    fn poll(&mut self, _: &mut Emitter) -> Result<()> {
        self.polled.fetch_add(1, Ordering::SeqCst);
        match self.polls.pop_front() {
            Some(Poll::Answer) => Ok(()),
            Some(Poll::Fail) => anyhow::bail!("no answer"),
            Some(Poll::FailAndIdle) | None => {
                self.active = false;
                self.idle.send(()).unwrap();
                anyhow::bail!("no answer")
            }
        }
    }

    fn poll_interval(&self) -> Option<Duration> {
        self.active.then_some(Duration::from_millis(1))
    }

    fn reset(&mut self) {
        self.active = false;
    }
}

fn run_on_a_thread(
    speaker: Scripted,
) -> (
    Sender<PlayerCmd>,
    Receiver<PlayerEvent>,
    thread::JoinHandle<()>,
) {
    let (cmds, rx) = mpsc::channel();
    let (tx, events) = mpsc::channel();
    let thread = thread::spawn(move || run(Box::new(speaker), &rx, Emitter::new(tx)));
    (cmds, events, thread)
}

#[test]
fn three_failed_polls_in_a_row_stop_the_album() {
    use Poll::{Answer, Fail};
    let (speaker, _idle) = Scripted::new(vec![Fail, Fail, Answer, Fail, Fail, Fail]);
    let polled = Arc::clone(&speaker.polled);
    let (cmds, events, thread) = run_on_a_thread(speaker);
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2)),
        Ok(PlayerEvent::Stopped)
    );
    drop(cmds);
    thread.join().unwrap();
    assert_eq!(
        polled.load(Ordering::SeqCst),
        6,
        "a poll that works restarts the count, and polling ends with the album"
    );
}

#[test]
fn a_command_that_works_restarts_the_failed_poll_count() {
    use Poll::{Fail, FailAndIdle};
    let (speaker, idle) = Scripted::new(vec![Fail, FailAndIdle, Fail, FailAndIdle]);
    let (cmds, events, thread) = run_on_a_thread(speaker);
    idle.recv_timeout(Duration::from_secs(2)).unwrap();
    cmds.send(PlayerCmd::Next).unwrap();
    idle.recv_timeout(Duration::from_secs(2)).unwrap();
    drop(cmds);
    thread.join().unwrap();
    assert_eq!(events.try_iter().collect::<Vec<_>>(), []);
}

#[test]
fn a_failed_command_drops_the_rest_of_its_batch() {
    let (mut speaker, _idle) = Scripted::new(vec![]);
    speaker.active = false;
    speaker.fail_commands = true;
    let handled = Arc::clone(&speaker.handled);
    let (cmds, rx) = mpsc::channel();
    for _ in 0..3 {
        cmds.send(PlayerCmd::TogglePause).unwrap();
    }
    drop(cmds);
    let (tx, events) = mpsc::channel();
    run(Box::new(speaker), &rx, Emitter::new(tx));
    assert_eq!(handled.load(Ordering::SeqCst), 1);
    assert_eq!(
        events.try_iter().collect::<Vec<_>>(),
        [PlayerEvent::Stopped]
    );
}
