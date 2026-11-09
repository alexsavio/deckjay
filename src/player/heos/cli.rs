//! The HEOS CLI protocol: one command line out, JSON lines back. Change
//! events come on the same lines while they are on; the only one kept is
//! `event/player_now_playing_progress` of the player we follow.

use std::fmt;
use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::Value;

/// For each read and write, and for the whole wait for one reply.
pub(super) const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a HEOS command did not succeed. After `Fail` the connection is still
/// good; after the other two its state is unknown and the caller drops it.
#[derive(Debug)]
pub(super) enum HeosError {
    /// The speaker answered `result: fail`.
    Fail {
        command: String,
        /// `None` when the reply names none or a non-number.
        eid: Option<u32>,
        text: String,
    },
    /// The socket failed: reset, closed, broken pipe or a timeout.
    Io(io::Error),
    /// A reply the program cannot read, no reply in time, or no `result` it knows.
    Other(anyhow::Error),
}

impl fmt::Display for HeosError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HeosError::Fail { command, eid, text } => {
                write!(f, "HEOS {command} failed: {text} (eid ")?;
                match eid {
                    Some(eid) => write!(f, "{eid})"),
                    None => write!(f, "?)"),
                }
            }
            HeosError::Io(err) => err.fmt(f),
            HeosError::Other(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for HeosError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HeosError::Io(err) => Some(err),
            HeosError::Fail { .. } | HeosError::Other(_) => None,
        }
    }
}

impl From<io::Error> for HeosError {
    fn from(err: io::Error) -> HeosError {
        HeosError::Io(err)
    }
}

impl From<anyhow::Error> for HeosError {
    fn from(err: anyhow::Error) -> HeosError {
        HeosError::Other(err)
    }
}

pub(super) struct Cli {
    reader: BufReader<TcpStream>,
    io_timeout: Duration,
    /// The player whose progress events are kept.
    follow: Option<i64>,
    progress: Option<NowPlaying>,
}

/// Where the player is in the current stream, from
/// `event/player_now_playing_progress`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NowPlaying {
    pub(super) position: Duration,
    /// `None` when the event says 0: an m4a with its index at the end.
    pub(super) duration: Option<Duration>,
}

impl Cli {
    pub(super) fn connect(host: &str, port: u16, io_timeout: Duration) -> Result<Cli> {
        let stream = super::super::connect(host, port, io_timeout)?;
        let mut cli = Cli {
            reader: BufReader::new(stream),
            io_timeout,
            follow: None,
            progress: None,
        };
        // The spec's start-up advice; it also keeps change events off this connection.
        cli.request("system/register_for_change_events", &[("enable", "off")])?;
        Ok(cli)
    }

    /// Change events on or off, for this connection only.
    pub(super) fn set_change_events(&mut self, on: bool) -> Result<(), HeosError> {
        let enable = if on { "on" } else { "off" };
        self.request("system/register_for_change_events", &[("enable", enable)])?;
        Ok(())
    }

    /// Keep the progress events of player `pid` from now on.
    pub(super) fn follow(&mut self, pid: i64) {
        self.follow = Some(pid);
    }

    /// The latest progress event of the followed player since the last call.
    pub(super) fn take_progress(&mut self) -> Option<NowPlaying> {
        self.progress.take()
    }

    pub(super) fn peer_ip(&self) -> Option<String> {
        let addr = self.reader.get_ref().peer_addr().ok()?;
        Some(addr.ip().to_string())
    }

    pub(super) fn players(&mut self) -> Result<Vec<PlayerInfo>> {
        let reply = self.request("player/get_players", &[])?;
        let raw: Vec<RawPlayer> =
            serde_json::from_value(reply.payload).context("unexpected HEOS player list")?;
        raw.into_iter().map(PlayerInfo::try_from).collect()
    }

    fn request(&mut self, command: &str, args: &[(&str, &str)]) -> Result<Reply, HeosError> {
        self.exchange(command, args)?.checked(command)
    }

    /// Sends `command` and reads lines until its reply. An error here leaves
    /// the connection in an unknown state, so the caller must drop it.
    pub(super) fn exchange(
        &mut self,
        command: &str,
        args: &[(&str, &str)],
    ) -> Result<Reply, HeosError> {
        let line = command_line(command, args);
        if line.trim_end().contains(['\r', '\n']) {
            return Err(anyhow!("a HEOS command cannot contain a line break").into());
        }
        self.reader.get_mut().write_all(line.as_bytes())?;
        let deadline = Instant::now() + self.io_timeout;
        loop {
            let mut buf = String::new();
            if self.reader.read_line(&mut buf)? == 0 {
                let closed = io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "the HEOS speaker closed the connection",
                );
                return Err(closed.into());
            }
            if !buf.trim().is_empty() {
                let reply: Reply = serde_json::from_str(buf.trim())
                    .with_context(|| format!("unexpected HEOS reply {:?}", buf.trim()))?;
                if reply.answers(command) {
                    return Ok(reply);
                }
                self.note(&reply);
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("no HEOS reply to {command}").into());
            }
        }
    }
}

impl Cli {
    /// Keeps a progress event of the followed player; drops every other line.
    fn note(&mut self, line: &Reply) {
        if unspaced(&line.heos.command) != "event/player_now_playing_progress" {
            return;
        }
        let message = line.message();
        let pid = message.get("pid").and_then(|pid| pid.parse::<i64>().ok());
        if pid.is_none() || pid != self.follow {
            return;
        }
        let millis = |key| {
            message
                .get(key)
                .and_then(|ms| ms.parse().ok())
                .map(Duration::from_millis)
        };
        if let Some(position) = millis("cur_pos") {
            self.progress = Some(NowPlaying {
                position,
                duration: millis("duration").filter(|d| !d.is_zero()),
            });
        }
    }
}

/// `url` goes last and unencoded, as the spec asks: the CLI takes the rest of
/// the line as the URL, so its own `&`, `=` and `%` stay as they are.
pub(super) fn command_line(command: &str, args: &[(&str, &str)]) -> String {
    let encoded = args
        .iter()
        .filter(|(key, _)| *key != "url")
        .map(|(key, value)| format!("{key}={}", encode(value)));
    let urls = args
        .iter()
        .filter(|(key, _)| *key == "url")
        .map(|(key, value)| format!("{key}={value}"));
    let query: Vec<String> = encoded.chain(urls).collect();
    if query.is_empty() {
        format!("heos://{command}\r\n")
    } else {
        format!("heos://{command}?{}\r\n", query.join("&"))
    }
}

/// One JSON line from the CLI: a reply or a change event.
#[derive(Deserialize)]
pub(super) struct Reply {
    heos: Header,
    #[serde(default)]
    payload: Value,
}

#[derive(Deserialize)]
struct Header {
    #[serde(default)]
    command: String,
    #[serde(default)]
    result: String,
    #[serde(default)]
    message: String,
}

impl Reply {
    /// False for change events, replies to other commands, and the "command
    /// under process" note that comes before a slow reply.
    pub(super) fn answers(&self, command: &str) -> bool {
        unspaced(&self.heos.command) == unspaced(command)
            && !self.heos.message.contains("command under process")
    }

    pub(super) fn checked(self, command: &str) -> Result<Reply, HeosError> {
        match unspaced(&self.heos.result).as_str() {
            "success" => Ok(self),
            "fail" => {
                let message = self.message();
                Err(HeosError::Fail {
                    command: command.to_string(),
                    eid: message.get("eid").and_then(|eid| eid.parse().ok()),
                    text: message.get("text").unwrap_or("no reason given").to_string(),
                })
            }
            other => Err(anyhow!("HEOS {command}: unexpected result {other:?}").into()),
        }
    }

    pub(super) fn message(&self) -> Message {
        Message::parse(&self.heos.message)
    }
}

/// The spec's examples pad names with spaces, even inside (`" player/ set_volume "`),
/// and wrap them in single quotes.
fn unspaced(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '\'')
        .collect()
}

pub(super) struct Message(Vec<(String, String)>);

impl Message {
    pub(super) fn parse(raw: &str) -> Message {
        Message(
            raw.split('&')
                .filter(|pair| !pair.trim().is_empty())
                .map(|pair| {
                    let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                    (decode(unquote(key)), decode(unquote(value)))
                })
                .collect(),
        )
    }

    pub(super) fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// The spec's examples quote values: `pid='1'`.
fn unquote(s: &str) -> &str {
    s.trim().trim_matches('\'').trim()
}

/// `&`, `=` and `%` are the only characters the CLI encodes.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("%26"),
            '=' => out.push_str("%3D"),
            '%' => out.push_str("%25"),
            _ => out.push(c),
        }
    }
    out
}

/// Single pass, so `%2526` becomes `%26` and not `&`.
fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let code = rest.get(i + 1..i + 3).map(str::to_ascii_uppercase);
        let decoded = match code.as_deref() {
            Some("26") => Some('&'),
            Some("3D") => Some('='),
            Some("25") => Some('%'),
            _ => None,
        };
        if let Some(c) = decoded {
            out.push(c);
            rest = &rest[i + 3..];
        } else {
            out.push('%');
            rest = &rest[i + 1..];
        }
    }
    out.push_str(rest);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerInfo {
    pub pid: i64,
    pub name: String,
    pub model: String,
    pub ip: Option<String>,
}

#[derive(Deserialize)]
struct RawPlayer {
    name: String,
    pid: Pid,
    #[serde(default)]
    model: String,
    #[serde(default)]
    ip: Option<String>,
}

/// `pid` comes as a JSON number or as a string.
#[derive(Deserialize)]
#[serde(untagged)]
enum Pid {
    Number(i64),
    Text(String),
}

impl TryFrom<RawPlayer> for PlayerInfo {
    type Error = anyhow::Error;

    fn try_from(raw: RawPlayer) -> Result<PlayerInfo> {
        let pid = match raw.pid {
            Pid::Number(pid) => pid,
            Pid::Text(pid) => unquote(&pid)
                .parse()
                .with_context(|| format!("bad HEOS player id {pid:?}"))?,
        };
        Ok(PlayerInfo {
            pid,
            name: decode(&raw.name),
            model: decode(&raw.model),
            ip: raw.ip,
        })
    }
}

pub fn players(host: &str, port: u16) -> Result<Vec<PlayerInfo>> {
    Cli::connect(host, port, IO_TIMEOUT)?.players()
}
