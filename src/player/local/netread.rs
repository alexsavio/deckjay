//! A radio station's stream as a [`Read`] for symphonia. A reader thread
//! pulls the HTTP body into a bounded buffer; [`NetRead::read`] takes from
//! it. [`NetRead::open`] fails for a station that sends nothing for
//! [`Limits::first_byte`]. Once it has sent something, a stream that ends,
//! breaks or sends nothing for [`Limits::idle`] is fetched again, at most
//! [`Limits::reconnects`] times in a row; after a connection that sent
//! nothing, the next one waits [`Limits::backoff`], doubled for each
//! reconnect before it.
//!
//! ureq has no timeout between two reads of a body, so a reader thread on a
//! stalled connection stays blocked until the server or the network drops
//! it, or until [`BODY_BUDGET`] runs out; the stream goes on with a new
//! thread and connection, and the old thread exits at its next read. The
//! budget also cuts a healthy stream every 3 h: that connection was
//! [`STEADY`], so it gets all the reconnects and no wait, and the stream
//! goes on after a short gap.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use tracing::{debug, warn};
use ureq::Agent;

/// Bytes per read from the network.
const CHUNK: usize = 16 * 1024;
/// Chunks waiting for the decoder: 256 KiB, 16 s of a 128 kbit/s stream.
const BUFFERED: usize = 16;
/// How often a read that waits for data looks for [`Cancel`].
const CANCEL_CHECK: Duration = Duration::from_millis(50);
/// A connection that delivered for this long counts as working, so the
/// next break gets all the reconnects again.
const STEADY: Duration = Duration::from_secs(30);
/// How long one connection's body is read: the only limit that frees a
/// reader thread stuck on a stalled connection.
const BODY_BUDGET: Duration = Duration::from_hours(3);

#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    /// A stream that sends nothing for this long is fetched again.
    pub(super) idle: Duration,
    /// A station that sends nothing for this long after its reply headers
    /// has failed: it is not fetched again.
    pub(super) first_byte: Duration,
    /// New connections after a break, before the stream counts as gone.
    pub(super) reconnects: u32,
    /// The wait before a new connection when the last one sent nothing;
    /// it doubles with each reconnect in a row.
    pub(super) backoff: Duration,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            idle: Duration::from_secs(10),
            first_byte: Duration::from_secs(5),
            reconnects: 3,
            backoff: Duration::from_secs(1),
        }
    }
}

/// Ends the stream from another thread: a read waiting for the network
/// returns end of stream within [`CANCEL_CHECK`].
#[derive(Clone, Debug, Default)]
pub(super) struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub(super) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub(super) struct NetRead {
    agent: Agent,
    url: String,
    limits: Limits,
    /// In a `Mutex` only so `NetRead` is `Sync`, as symphonia wants; `read`
    /// has `&mut self` and never locks it.
    chunks: Mutex<Receiver<Vec<u8>>>,
    chunk: Vec<u8>,
    /// Bytes of `chunk` already read.
    taken: usize,
    reconnects: u32,
    connected: Instant,
    /// The stream sent its first bytes, on any connection.
    started: bool,
    /// The current connection sent bytes.
    delivered: bool,
    cancel: Cancel,
}

impl NetRead {
    /// Connects to `url` and waits for its first bytes; fails when it
    /// cannot be reached, does not answer 2xx, or sends nothing for
    /// [`Limits::first_byte`].
    pub(super) fn open(url: &str, limits: Limits) -> Result<NetRead> {
        let agent = crate::net::stream_agent();
        let body = get(&agent, url)?;
        let (tx, chunks) = mpsc::sync_channel(BUFFERED);
        start(move || pump(body, &tx))?;
        let mut stream = NetRead {
            agent,
            url: url.to_string(),
            limits,
            chunks: Mutex::new(chunks),
            chunk: Vec::new(),
            taken: 0,
            reconnects: 0,
            connected: Instant::now(),
            started: false,
            delivered: false,
            cancel: Cancel::default(),
        };
        // Here, not in the first read: symphonia's probe turns a read error
        // into "no suitable format reader found".
        if let Some(chunk) = stream.next_chunk()? {
            stream.chunk = chunk;
        }
        Ok(stream)
    }

    pub(super) fn cancel_handle(&self) -> Cancel {
        self.cancel.clone()
    }

    /// The next chunk from the network; `None` once cancelled.
    fn next_chunk(&mut self) -> io::Result<Option<Vec<u8>>> {
        let patience = if self.started {
            self.limits.idle
        } else {
            self.limits.first_byte
        };
        let mut deadline = Instant::now() + patience;
        loop {
            // First in the loop: it also ends a reconnect cancelled in its wait.
            if self.cancel.is_cancelled() {
                return Ok(None);
            }
            let now = Instant::now();
            if now >= deadline {
                self.reconnect("the station sent nothing")?;
                deadline = Instant::now() + self.limits.idle;
                continue;
            }
            let chunks = self
                .chunks
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner);
            match chunks.recv_timeout((deadline - now).min(CANCEL_CHECK)) {
                Ok(chunk) => {
                    self.started = true;
                    self.delivered = true;
                    return Ok(Some(chunk));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.reconnect("the stream ended")?;
                    deadline = Instant::now() + self.limits.idle;
                }
            }
        }
    }

    /// Fetches the stream again on a new reader thread, which drops its
    /// sender, so the next wait sees a break, when the fetch fails. Returns
    /// without connecting when cancelled during the backoff.
    fn reconnect(&mut self, why: &str) -> io::Result<()> {
        if !self.started {
            return Err(io::Error::other("the station sent nothing"));
        }
        if self.connected.elapsed() >= STEADY {
            self.reconnects = 0;
        }
        if self.reconnects >= self.limits.reconnects {
            return Err(io::Error::other(format!(
                "{why}, and {} new connections did not help",
                self.reconnects
            )));
        }
        if !self.delivered && !self.wait(self.limits.backoff * (1 << self.reconnects)) {
            return Ok(());
        }
        self.reconnects += 1;
        warn!(attempt = self.reconnects, "{why}; connecting again");
        let (tx, chunks) = mpsc::sync_channel(BUFFERED);
        let (agent, url) = (self.agent.clone(), self.url.clone());
        start(move || match get(&agent, &url) {
            Ok(body) => pump(body, &tx),
            Err(err) => debug!("cannot connect to the station again: {err:#}"),
        })
        .map_err(io::Error::other)?;
        *self
            .chunks
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) = chunks;
        self.connected = Instant::now();
        self.delivered = false;
        Ok(())
    }

    /// Sleeps for `time`; false when cancelled first.
    fn wait(&self, time: Duration) -> bool {
        let until = Instant::now() + time;
        loop {
            if self.cancel.is_cancelled() {
                return false;
            }
            let now = Instant::now();
            if now >= until {
                return true;
            }
            thread::sleep((until - now).min(CANCEL_CHECK));
        }
    }
}

impl Read for NetRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.taken == self.chunk.len() {
            match self.next_chunk()? {
                Some(chunk) => {
                    self.chunk = chunk;
                    self.taken = 0;
                }
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.chunk.len() - self.taken);
        buf[..n].copy_from_slice(&self.chunk[self.taken..self.taken + n]);
        self.taken += n;
        Ok(n)
    }
}

fn get(agent: &Agent, url: &str) -> Result<impl Read + Send + 'static> {
    let reply = agent
        .get(url)
        .config()
        .timeout_recv_body(Some(BODY_BUDGET))
        .build()
        .call()
        .context("cannot reach the station's stream")?;
    let status = reply.status();
    ensure!(
        status.is_success(),
        "the station's stream answered {status}"
    );
    Ok(reply.into_body().into_reader())
}

fn start(work: impl FnOnce() + Send + 'static) -> Result<()> {
    thread::Builder::new()
        .name("radio".into())
        .spawn(work)
        .context("cannot start the radio thread")?;
    Ok(())
}

/// Copies `body` into `chunks` until it ends, breaks, or nobody reads.
fn pump(mut body: impl Read, chunks: &SyncSender<Vec<u8>>) {
    loop {
        let mut chunk = vec![0; CHUNK];
        match body.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                chunk.truncate(n);
                if chunks.send(chunk).is_err() {
                    return;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                debug!("the station's stream broke: {err}");
                return;
            }
        }
    }
}
