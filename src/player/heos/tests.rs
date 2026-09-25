use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

use super::cli::{PlayerInfo, Reply, command_line};
use super::*;

const ONE_PLAYER: &str = r#"[{"name": "Kitchen", "pid": 7, "model": "HEOS 1", "version": "1.583.147", "network": "wifi", "ip": "10.0.0.9"}]"#;
/// String pids; the second player's ip is the host the tests connect to.
const TWO_PLAYERS: &str = concat!(
    r#"[{"name": "Kitchen", "pid": "12", "model": "HEOS 1", "ip": "10.0.0.9"},"#,
    r#" {"name": "Kids %26 Co", "pid": "-1234", "model": "HEOS 7", "ip": "127.0.0.1"}]"#
);

/// What the fake speaker does and what it was sent.
#[derive(Default)]
struct Script {
    commands: Vec<String>,
    state: &'static str,
    /// Answer this command with `result: fail`.
    fail: Option<&'static str>,
    /// Send a change event and a "command under process" note before each reply.
    noisy: bool,
    /// Close the connection instead of answering the next command.
    hang_up: bool,
    /// Never answer this command; the connection stays open.
    mute: Option<&'static str>,
}

/// A HEOS CLI on 127.0.0.1 that answers with canned JSON.
struct FakeHeos {
    port: u16,
    script: Arc<Mutex<Script>>,
}

impl FakeHeos {
    fn start(players: &'static str) -> FakeHeos {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let script = Arc::new(Mutex::new(Script {
            state: "stop",
            ..Script::default()
        }));
        let shared = Arc::clone(&script);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let script = Arc::clone(&shared);
                thread::spawn(move || serve(stream, &script, players));
            }
        });
        FakeHeos { port, script }
    }

    fn script(&self) -> std::sync::MutexGuard<'_, Script> {
        self.script.lock().unwrap()
    }

    fn commands(&self) -> Vec<String> {
        self.script().commands.clone()
    }

    fn last_command(&self) -> String {
        self.commands().pop().unwrap()
    }

    /// The URLs sent with `play_stream`, in order.
    fn streamed(&self) -> Vec<String> {
        self.commands()
            .iter()
            .filter(|c| c.starts_with("browse/play_stream?"))
            .map(|c| c.split_once("&url=").unwrap().1.to_string())
            .collect()
    }
}

fn serve(stream: TcpStream, script: &Mutex<Script>, players: &str) {
    let mut writer = stream.try_clone().unwrap();
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        let command = line.strip_prefix("heos://").unwrap().to_string();
        let replies = {
            let mut script = script.lock().unwrap();
            script.commands.push(command.clone());
            if std::mem::take(&mut script.hang_up) {
                return;
            }
            if script.mute.is_some_and(|name| command.starts_with(name)) {
                continue;
            }
            answer(&mut script, &command, players)
        };
        for reply in replies {
            writer.write_all(format!("{reply}\r\n").as_bytes()).unwrap();
        }
    }
}

fn answer(script: &mut Script, command: &str, players: &str) -> Vec<String> {
    let (name, query) = command.split_once('?').unwrap_or((command, ""));
    let pid = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("pid="))
        .unwrap_or("");
    // Echoed the way the spec's examples print it: " player/ set_volume ".
    let echoed = format!(" {} ", name.replacen('/', "/ ", 1));
    let mut lines = Vec::new();
    if script.noisy {
        lines.push(r#"{"heos": {"command": "event/player_state_changed", "message": "pid=7&state=pause"}}"#.to_string());
        lines.push(format!(
            r#"{{"heos": {{"command": "{name}", "result": "success", "message": "command under process"}}}}"#
        ));
    }
    if name == "player/clear_queue" {
        // What a Denon AVR-X1600H answers first, before the real reply.
        lines.push(format!(
            r#"{{"heos": {{"command": "{name}", "result": "success", "message": "command under process&pid={pid}"}}}}"#
        ));
    }
    let reply = if script.fail == Some(name) {
        format!(
            r#"{{"heos": {{"command": "{name}", "result": "fail", "message": "eid=14&text=cannot play&{query}"}}}}"#
        )
    } else {
        match name {
            "player/get_players" => format!(
                r#"{{"heos": {{"command": "{name}", "result": "success", "message": ""}}, "payload": {players}}}"#
            ),
            "player/get_play_state" => format!(
                r#"{{"heos": {{"command": "{echoed}", "result": "success", "message": "pid='{pid}'&state='{}'"}}}}"#,
                script.state
            ),
            _ => {
                if name == "player/set_play_state" {
                    script.state = ["play", "pause", "stop"]
                        .into_iter()
                        .find(|s| query.ends_with(&format!("state={s}")))
                        .unwrap();
                }
                format!(
                    r#"{{"heos": {{"command": "{echoed}", "result": "success", "message": "{query}"}}}}"#
                )
            }
        }
    };
    lines.push(reply);
    lines
}

fn track_url(i: usize) -> String {
    format!(
        "http://10.0.0.2:8765/music/01%20High%20Tones/{:02}%20Tone%20880%20Hz.m4a",
        i + 1
    )
}

fn tracks(n: usize) -> Vec<TrackInfo> {
    (0..n)
        .map(|i| TrackInfo {
            url: track_url(i),
            path: format!("High Tones/{i:02}.m4a").into(),
            content_type: "audio/mp4".into(),
            title: format!("Tone {i}"),
            album: "High Tones".into(),
            cover_url: None,
        })
        .collect()
}

fn play_album(n: usize) -> PlayerCmd {
    PlayerCmd::PlayAlbum {
        album: 3,
        tracks: tracks(n),
        volume: 0.2,
    }
}

/// The real player against the fake speaker, driven the way `player::run` drives it.
struct Rig {
    fake: FakeHeos,
    player: HeosPlayer,
    emitter: Emitter,
    events: Receiver<PlayerEvent>,
}

impl Rig {
    fn new(players: &'static str) -> Rig {
        let fake = FakeHeos::start(players);
        let (tx, events) = mpsc::channel();
        Rig {
            player: HeosPlayer::new("127.0.0.1".into(), fake.port),
            fake,
            emitter: Emitter { tx, last: None },
            events,
        }
    }

    fn send(&mut self, cmd: PlayerCmd) -> Result<()> {
        self.emitter.last = None;
        self.player.handle(cmd, &mut self.emitter)
    }

    fn poll_with(&mut self, state: &'static str) {
        self.fake.script().state = state;
        self.player.poll(&mut self.emitter).unwrap();
    }

    fn events(&self) -> Vec<PlayerEvent> {
        self.events.try_iter().collect()
    }
}

use PlayerEvent::{Paused, Playing, Stopped};

#[test]
fn play_album_sets_the_volume_then_streams_the_first_track() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(play_album(3)).unwrap();
    assert_eq!(
        rig.fake.commands(),
        [
            "system/register_for_change_events?enable=off",
            "player/get_players",
            "player/set_volume?pid=7&level=20",
            "player/clear_queue?pid=7",
            format!("browse/play_stream?pid=7&url={}", track_url(0)).as_str(),
        ]
    );
    assert_eq!(rig.events(), [Playing(3)]);
    assert_eq!(rig.player.poll_interval(), Some(POLL_INTERVAL));
}

#[test]
fn a_track_that_ended_starts_the_next() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(play_album(3)).unwrap();
    rig.poll_with("play");
    rig.poll_with("stop");
    assert_eq!(rig.fake.streamed(), [track_url(0), track_url(1)]);
    assert_eq!(rig.events(), [Playing(3)]);
    assert_eq!(rig.player.poll_interval(), Some(POLL_INTERVAL));
}

#[test]
fn a_stop_while_the_track_loads_waits_then_gives_up() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(play_album(3)).unwrap();
    rig.poll_with("stop");
    assert_eq!(rig.fake.streamed(), [track_url(0)]);
    assert_eq!(rig.events(), [Playing(3)]);

    rig.player.load_timeout = Duration::ZERO;
    rig.poll_with("stop");
    assert_eq!(rig.fake.streamed(), [track_url(0)]);
    assert_eq!(rig.events(), [Stopped]);
    assert_eq!(rig.player.poll_interval(), None);
}

#[test]
fn the_last_track_ending_stops_the_album() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(play_album(2)).unwrap();
    rig.send(PlayerCmd::Next).unwrap();
    rig.poll_with("play");
    rig.poll_with("stop");
    assert_eq!(rig.fake.streamed(), [track_url(0), track_url(1)]);
    assert_eq!(rig.events(), [Playing(3), Playing(3), Stopped]);
    assert_eq!(rig.player.poll_interval(), None);
    // Otherwise the speaker keeps retrying the finished stream.
    assert_eq!(
        rig.fake.last_command(),
        "player/set_play_state?pid=7&state=stop"
    );
}

#[test]
fn unknown_counts_as_not_playing() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(play_album(3)).unwrap();
    rig.poll_with("unknown");
    assert_eq!(rig.fake.streamed(), [track_url(0)]);
    assert_eq!(rig.player.poll_interval(), Some(POLL_INTERVAL));

    rig.poll_with("play");
    rig.poll_with("unknown");
    assert_eq!(rig.fake.streamed(), [track_url(0), track_url(1)]);
    assert_eq!(rig.events(), [Playing(3)]);
}

#[test]
fn toggle_pause_pauses_and_resumes() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(play_album(3)).unwrap();
    rig.poll_with("play");

    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(
        rig.fake.last_command(),
        "player/set_play_state?pid=7&state=pause"
    );
    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(
        rig.fake.last_command(),
        "player/set_play_state?pid=7&state=play"
    );
    assert_eq!(rig.events(), [Playing(3), Paused(3), Playing(3)]);
}

#[test]
fn toggle_pause_restarts_an_album_that_ended() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(rig.events(), [Stopped], "no album yet: nothing to play");

    rig.send(play_album(1)).unwrap();
    rig.poll_with("play");
    rig.poll_with("stop");
    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(rig.fake.streamed(), [track_url(0), track_url(0)]);
    assert_eq!(rig.events(), [Playing(3), Stopped, Playing(3)]);
}

#[test]
fn toggle_pause_while_a_track_loads_keeps_it() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(play_album(3)).unwrap();
    rig.send(PlayerCmd::Next).unwrap();
    rig.send(PlayerCmd::TogglePause).unwrap();
    assert_eq!(rig.fake.streamed(), [track_url(0), track_url(1)]);
    assert_eq!(rig.fake.last_command(), "player/get_play_state?pid=7");
    assert_eq!(rig.events(), [Playing(3), Playing(3), Playing(3)]);
}

#[test]
fn next_and_prev_pick_the_right_track() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(play_album(3)).unwrap();
    for cmd in [
        PlayerCmd::Next,
        PlayerCmd::Next,
        PlayerCmd::Next, // on the last track: nothing
        PlayerCmd::Prev,
        PlayerCmd::Prev,
        PlayerCmd::Prev, // on the first track: restarts it
    ] {
        rig.send(cmd).unwrap();
    }
    assert_eq!(rig.fake.streamed(), [0, 1, 2, 1, 0, 0].map(track_url));
}

#[test]
fn set_volume_sends_a_percentage() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(PlayerCmd::SetVolume(0.35)).unwrap();
    assert_eq!(rig.fake.last_command(), "player/set_volume?pid=7&level=35");
}

#[test]
fn an_empty_queue_does_not_stop_playback() {
    let mut rig = Rig::new(ONE_PLAYER);
    // A Denon AVR-X1600H answers eid 4 when the queue is already empty.
    rig.fake.script().fail = Some("player/clear_queue");
    rig.send(play_album(3)).unwrap();
    assert_eq!(rig.fake.streamed(), [track_url(0)]);
    assert_eq!(rig.events(), [Playing(3)]);
}

#[test]
fn a_fail_reply_is_an_error_but_keeps_the_connection() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.fake.script().fail = Some("browse/play_stream");
    let err = rig.send(play_album(3)).unwrap_err();
    assert!(format!("{err:#}").contains("cannot play"), "{err:#}");
    assert_eq!(rig.player.poll_interval(), None);

    rig.send(PlayerCmd::SetVolume(0.5)).unwrap();
    let connects = rig
        .fake
        .commands()
        .iter()
        .filter(|c| c.starts_with("system/register_for_change_events"))
        .count();
    assert_eq!(connects, 1);
}

#[test]
fn the_player_loop_reports_stopped_when_a_command_fails() {
    let fake = FakeHeos::start(ONE_PLAYER);
    fake.script().fail = Some("browse/play_stream");
    let (tx, events) = mpsc::channel();
    let output = super::super::Output::Heos {
        host: "127.0.0.1".into(),
        port: fake.port,
    };
    let player = super::super::spawn(output, tx);
    player.send(play_album(3)).unwrap();
    assert_eq!(events.recv_timeout(IO_TIMEOUT), Ok(Stopped));
}

#[test]
fn a_dropped_connection_is_reopened_and_the_command_sent_again() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(PlayerCmd::SetVolume(0.5)).unwrap();
    rig.fake.script().hang_up = true;
    rig.send(PlayerCmd::SetVolume(0.6)).unwrap();
    assert_eq!(
        rig.fake.commands()[3..],
        [
            "player/set_volume?pid=7&level=60",
            "system/register_for_change_events?enable=off",
            "player/get_players",
            "player/set_volume?pid=7&level=60",
        ]
    );
}

#[test]
fn a_timed_out_command_is_not_sent_again() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.player.io_timeout = Duration::from_millis(100);
    rig.send(PlayerCmd::SetVolume(0.5)).unwrap();
    rig.fake.script().mute = Some("player/set_volume");
    assert!(rig.send(PlayerCmd::SetVolume(0.6)).is_err());
    assert_eq!(rig.fake.last_command(), "player/set_volume?pid=7&level=60");
}

#[test]
fn a_silent_cli_fails_within_the_io_timeout() {
    // The kernel accepts the connection; nothing ever answers.
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut player = HeosPlayer::new("127.0.0.1".into(), silent.local_addr().unwrap().port());
    player.io_timeout = Duration::from_millis(100);
    let (tx, _events) = mpsc::channel();
    let start = Instant::now();
    let result = player.handle(PlayerCmd::SetVolume(0.5), &mut Emitter { tx, last: None });
    assert!(result.is_err());
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn a_timed_out_clear_queue_stops_the_track_change() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.player.io_timeout = Duration::from_millis(100);
    rig.send(play_album(3)).unwrap();
    rig.fake.script().mute = Some("player/clear_queue");
    assert!(rig.send(PlayerCmd::Next).is_err());
    assert_eq!(
        rig.fake.streamed(),
        [track_url(0)],
        "no stream goes out without its clear_queue"
    );
}

#[test]
fn a_poll_that_sees_pause_reports_it_and_counts_the_track_as_started() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(play_album(3)).unwrap();
    rig.poll_with("pause");
    rig.poll_with("stop");
    assert_eq!(rig.fake.streamed(), [track_url(0), track_url(1)]);
    assert_eq!(rig.events(), [Playing(3), Paused(3), Playing(3)]);
}

#[test]
fn a_failed_play_stream_from_a_poll_ends_the_album() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(play_album(3)).unwrap();
    rig.poll_with("play");
    rig.fake.script().fail = Some("browse/play_stream");
    rig.fake.script().state = "stop";
    assert!(rig.player.poll(&mut rig.emitter).is_err());
    assert_eq!(rig.events(), [Playing(3), Stopped]);
    assert_eq!(rig.player.poll_interval(), None);
    assert_eq!(
        rig.fake.last_command(),
        "player/set_play_state?pid=7&state=stop"
    );
}

#[test]
fn next_and_prev_without_an_album_send_nothing() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.send(PlayerCmd::Next).unwrap();
    rig.send(PlayerCmd::Prev).unwrap();
    assert_eq!(rig.fake.commands(), Vec::<String>::new());
    assert_eq!(rig.events(), []);
}

#[test]
fn an_album_without_tracks_is_an_error() {
    let mut rig = Rig::new(ONE_PLAYER);
    let err = rig.send(play_album(0)).unwrap_err();
    assert!(format!("{err:#}").contains("no track 0"), "{err:#}");
    assert_eq!(rig.fake.streamed(), Vec::<String>::new());
}

#[test]
fn events_and_under_process_notes_before_a_reply_are_skipped() {
    let mut rig = Rig::new(ONE_PLAYER);
    rig.fake.script().noisy = true;
    rig.send(play_album(3)).unwrap();
    // The event line says "pause"; only the real reply counts.
    rig.poll_with("play");
    assert_eq!(rig.fake.streamed(), [track_url(0)]);
    assert_eq!(rig.events(), [Playing(3)]);
}

#[test]
fn players_lists_names_pids_and_ips() {
    let fake = FakeHeos::start(concat!(
        r#"[{"name": "Kitchen", "pid": 7, "model": "HEOS 1", "ip": "10.0.0.9"},"#,
        r#" {"name": "Kids %26 Co", "pid": "-1234", "model": "HEOS 7"},"#,
        r#" {"name": "Hall", "pid": -5, "model": "HEOS 3", "ip": "10.0.0.11"}]"#
    ));
    let player = |pid, name: &str, model: &str, ip: Option<&str>| PlayerInfo {
        pid,
        name: name.into(),
        model: model.into(),
        ip: ip.map(Into::into),
    };
    assert_eq!(
        players("127.0.0.1", fake.port).unwrap(),
        [
            player(7, "Kitchen", "HEOS 1", Some("10.0.0.9")),
            player(-1234, "Kids & Co", "HEOS 7", None),
            player(-5, "Hall", "HEOS 3", Some("10.0.0.11")),
        ]
    );
}

#[test]
fn the_player_whose_ip_is_the_host_is_used() {
    let mut rig = Rig::new(TWO_PLAYERS);
    rig.send(PlayerCmd::SetVolume(0.5)).unwrap();
    assert_eq!(
        rig.fake.last_command(),
        "player/set_volume?pid=-1234&level=50"
    );
}

#[test]
fn a_system_without_players_is_an_error() {
    let mut rig = Rig::new("[]");
    let err = rig.send(PlayerCmd::SetVolume(0.5)).unwrap_err();
    assert!(format!("{err:#}").contains("no players"), "{err:#}");
}

#[test]
fn messages_are_unquoted_and_decoded() {
    let message = Message::parse(" pid='-3' & state='play'&text=a%26b%3Dc%25d %2526&flag");
    assert_eq!(message.get("pid"), Some("-3"));
    assert_eq!(message.get("state"), Some("play"));
    assert_eq!(message.get("text"), Some("a&b=c%d %26"));
    assert_eq!(message.get("flag"), Some(""));
    assert_eq!(message.get("eid"), None);
}

#[test]
fn reply_commands_match_despite_spaces_and_quotes() {
    let reply: Reply = serde_json::from_str(
        r#"{"heos": {"command": " player/ set_volume ", "result": "success", "message": "pid='1'&level='20'"}}"#,
    )
    .unwrap();
    assert!(reply.answers("player/set_volume"));
    assert!(!reply.answers("player/get_volume"));
    assert_eq!(reply.message().get("level"), Some("20"));
}

#[test]
fn command_lines_encode_values_but_put_the_url_last_and_raw() {
    assert_eq!(
        command_line(
            "browse/play_stream",
            &[("url", "http://h/a%20b?x=1&y=2"), ("pid", "a&b=c%")]
        ),
        "heos://browse/play_stream?pid=a%26b%3Dc%25&url=http://h/a%20b?x=1&y=2\r\n"
    );
    assert_eq!(
        command_line("player/get_players", &[]),
        "heos://player/get_players\r\n"
    );
}
