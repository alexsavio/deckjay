//! `kids-deck spotify-login`: signs in once in a browser and saves the refresh
//! token. Spotify sends the browser back to a listener on this machine; on a
//! machine without a browser, the address the browser ends up at can be
//! pasted instead.

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use tracing::debug;

use super::api::{Client, Device, Endpoints};
use super::auth::{self, Pkce};
use super::token::{Secret, TokenFile};

const TIMEOUT: Duration = Duration::from_mins(5);
const CALLBACK_PATH: &str = "/callback";
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// A browser may open a connection and send nothing on it.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REQUEST_HEAD: u64 = 16 * 1024;

const DONE_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>kids-deck</title>\
    <p>Done: kids-deck may now control Spotify. You can close this tab.</p>";
const FAILED_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>kids-deck</title>\
    <p>The sign-in did not work; the terminal says why. You can close this tab.</p>";
const NOT_FOUND_PAGE: &str = "<!doctype html><title>kids-deck</title><p>Not found.</p>";

/// Signs in and saves `<state_dir>/spotify-token.json`, then prints the account
/// name and the Connect devices Spotify sees.
///
/// `listen` is where the browser comes back to, normally `127.0.0.1:8898`; the
/// redirect URI is always `http://127.0.0.1:<port>/callback` (Spotify refuses
/// `localhost`), so over SSH forward that port. `answer` is where a pasted
/// address comes from, normally stdin; the thread reading it may outlive this call.
pub fn run(
    client_id: &str,
    state_dir: &Path,
    listen: SocketAddr,
    answer: impl BufRead + Send + 'static,
    endpoints: Endpoints,
    out: &mut impl Write,
) -> Result<()> {
    sign_in(
        client_id, state_dir, listen, answer, endpoints, out, TIMEOUT,
    )
}

fn sign_in(
    client_id: &str,
    state_dir: &Path,
    listen: SocketAddr,
    answer: impl BufRead + Send + 'static,
    endpoints: Endpoints,
    out: &mut impl Write,
    timeout: Duration,
) -> Result<()> {
    // Bound first: the redirect URI needs the port.
    let listener = TcpListener::bind(listen).with_context(|| {
        format!("cannot listen on {listen} for the answer from Spotify (in use? try --listen)")
    })?;
    let redirect_uri = format!(
        "http://127.0.0.1:{}{CALLBACK_PATH}",
        listener.local_addr()?.port()
    );
    let pkce = Pkce::new()?;
    let state = auth::new_state()?;
    writeln!(
        out,
        "Open this address in a browser, sign in to Spotify and allow kids-deck:\n\n{}\n",
        auth::authorize_url(client_id, &redirect_uri, &pkce.challenge, &state)
    )?;
    writeln!(
        out,
        "Waiting for the browser to come back to {redirect_uri}.\n\
         No browser on this machine? Open the address on another one, then paste \
         the address it ends up at (it starts with {redirect_uri}) here:"
    )?;
    out.flush()?;

    let code = wait_for_code(listener, answer, &state, &redirect_uri, timeout, out)?;
    let agent = crate::net::api_agent(Duration::from_secs(10));
    let tokens = auth::exchange_code(
        &agent,
        &endpoints.accounts,
        client_id,
        &code,
        &redirect_uri,
        &pkce.verifier,
    )
    .context("Spotify did not accept the sign-in")?;
    let token = TokenFile {
        client_id: client_id.into(),
        refresh_token: tokens.refresh.context("Spotify sent no refresh token")?,
    };
    token.save(state_dir)?;
    writeln!(
        out,
        "Signed in. The login is saved in {}.",
        TokenFile::path(state_dir).display()
    )?;
    let mut client =
        Client::new(token, state_dir, endpoints).with_access(tokens.access, tokens.expires_in);
    print_account(&mut client, out)
}

/// The account name and the Connect devices, for the sign-in and for checks.
/// A failed request is printed, not returned: the login itself is fine.
pub fn print_account(client: &mut Client, out: &mut impl Write) -> Result<()> {
    match client.me() {
        Ok(name) => writeln!(out, "Spotify account: {name}")?,
        Err(err) => writeln!(out, "Cannot read the Spotify account: {err:#}")?,
    }
    match client.devices() {
        Ok(devices) if devices.is_empty() => writeln!(
            out,
            "Spotify sees no Connect devices right now. Asleep? Open the Spotify app \
             once and play something on the speaker."
        )?,
        Ok(devices) => {
            writeln!(out, "Spotify Connect devices:")?;
            for device in &devices {
                writeln!(out, "  {}", describe(device))?;
            }
        }
        Err(err) => writeln!(out, "Cannot list the Spotify Connect devices: {err:#}")?,
    }
    Ok(())
}

fn describe(device: &Device) -> String {
    let mut notes = vec![device.kind.as_str()];
    if device.is_active {
        notes.push("active");
    }
    if device.is_restricted {
        notes.push("restricted: kids-deck cannot control it");
    }
    format!("{} ({})", device.name, notes.join(", "))
}

enum Heard {
    /// From the browser or a pasted address: the sign-in code, or why not.
    Answer(Result<Secret>),
    /// A pasted line that is not the address from the browser.
    Unusable,
}

fn wait_for_code(
    listener: TcpListener,
    answer: impl BufRead + Send + 'static,
    state: &str,
    redirect_uri: &str,
    timeout: Duration,
    out: &mut impl Write,
) -> Result<Secret> {
    let deadline = Instant::now() + timeout;
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    listener.set_nonblocking(true)?;
    let listening = {
        let (state, stop, tx) = (state.to_owned(), Arc::clone(&stop), tx.clone());
        thread::spawn(move || listen(&listener, &state, deadline, &stop, &tx))
    };
    {
        let state = state.to_owned();
        thread::spawn(move || read_pasted(answer, &state, &tx));
    }

    let heard = loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Heard::Answer(code)) => break code,
            Ok(Heard::Unusable) => {
                writeln!(
                    out,
                    "That is not the address from the browser: paste all of it, \
                     starting with {redirect_uri}"
                )?;
                out.flush()?;
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                break Err(anyhow!(
                    "no answer from Spotify after {}: run `kids-deck spotify-login` again",
                    minutes_or_seconds(timeout)
                ));
            }
        }
    };
    stop.store(true, Ordering::Relaxed);
    let _ = listening.join();
    heard
}

fn minutes_or_seconds(duration: Duration) -> String {
    match duration.as_secs() {
        secs if secs >= 60 => format!("{} minutes", secs / 60),
        secs => format!("{secs} s"),
    }
}

/// Serves each connection on its own thread, so an idle one cannot hold up
/// the one that carries the answer.
fn listen(
    listener: &TcpListener,
    state: &str,
    deadline: Instant,
    stop: &AtomicBool,
    tx: &Sender<Heard>,
) {
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        match listener.accept() {
            Ok((stream, _)) => {
                let (state, tx) = (state.to_owned(), tx.clone());
                thread::spawn(move || {
                    if let Some(code) = serve(&stream, &state) {
                        let _ = tx.send(Heard::Answer(code));
                    }
                });
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => thread::sleep(ACCEPT_POLL),
            Err(err) => {
                debug!("spotify-login: accept failed: {err}");
                thread::sleep(ACCEPT_POLL);
            }
        }
    }
}

/// Answers one HTTP request; `None` unless it was `GET /callback?…`.
fn serve(stream: &TcpStream, state: &str) -> Option<Result<Secret>> {
    // On macOS an accepted socket inherits the listener's non-blocking mode.
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    let mut head = BufReader::new(stream.take(MAX_REQUEST_HEAD));
    let mut request_line = String::new();
    head.read_line(&mut request_line).ok()?;
    // Read the headers too: closing a socket with unread data resets the
    // connection, and the browser would show an error instead of the page.
    let mut header = String::new();
    while head.read_line(&mut header).is_ok_and(|n| n > 0) && header.trim_end() != "" {
        header.clear();
    }

    let query = callback_target(&request_line);
    let Some(query) = query else {
        respond(stream, "404 Not Found", NOT_FOUND_PAGE);
        return None;
    };
    let code = auth::parse_callback(query, state);
    match code {
        Ok(_) => respond(stream, "200 OK", DONE_PAGE),
        Err(_) => respond(stream, "400 Bad Request", FAILED_PAGE),
    }
    Some(code)
}

/// The query of `GET /callback?… HTTP/1.1`.
fn callback_target(request_line: &str) -> Option<&str> {
    let mut parts = request_line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    (path == CALLBACK_PATH).then_some(query)
}

fn respond(mut stream: &TcpStream, status: &str, page: &str) {
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
}

fn read_pasted(answer: impl BufRead, state: &str, tx: &Sender<Heard>) {
    for line in answer.lines() {
        let Ok(line) = line else { return };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let heard = match pasted_query(line) {
            Some(query) => Heard::Answer(auth::parse_callback(query, state)),
            None => Heard::Unusable,
        };
        let done = matches!(heard, Heard::Answer(_));
        if tx.send(heard).is_err() || done {
            return;
        }
    }
}

/// The query of a pasted `http://127.0.0.1:<port>/callback?…` address.
fn pasted_query(line: &str) -> Option<&str> {
    let (address, rest) = line.split_once('?')?;
    let query = rest.split_once('#').map_or(rest, |(query, _)| query);
    address.ends_with(CALLBACK_PATH).then_some(query)
}

#[cfg(test)]
mod tests;
