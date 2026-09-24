//! kids-deck: a music player for kids.
//!
//! Album covers are shown on an Elgato Stream Deck. Pressing a cover plays
//! the album on a network speaker (Chromecast or HEOS). The program serves the
//! music folder over HTTP and tells the speaker to fetch the tracks from it.
//!
//! Three threads work together:
//!
//! - main: finds the deck, draws the keys and reacts to key presses
//!   ([`ui::Ui`], [`deck::Deck`]).
//! - `cast` or `heos`: sends commands to the speaker and reports its state
//!   ([`player::spawn`]).
//! - `http`: serves the music files to the speaker ([`server::spawn`]).
//!
//! `kids-deck simulator` runs something else: [`simulator`], a web page that
//! stands in for the Stream Deck, so the player can run without the hardware.

mod config;
mod deck;
mod icons;
mod library;
mod player;
mod server;
mod simulator;
mod ui;

use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use hidapi::HidApi;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::config::{Config, SpeakerType};
use crate::deck::Deck;
use crate::simulator::Model;
use crate::ui::Ui;

const USAGE: &str = "\
usage: kids-deck [CONFIG] [--simulator URL] [--advertise-host HOST]
                 [--check | --preview FILE.png]
       kids-deck simulator [--model NAME] [--port PORT]

  CONFIG              path to config.toml (default: ./config.toml)
  --simulator URL     use the deck simulator at URL, e.g. http://localhost:8090,
                      instead of a USB Stream Deck
  --advertise-host HOST
                      the address the speaker uses to reach this program;
                      overrides advertise_host in the config
  --check             list albums, Stream Decks and speaker status, then exit
  --preview FILE.png  draw the 15-key layout into a picture, then exit

  simulator           run the web Stream Deck simulator
  --model NAME        mk2 (default), mini, neo, xl or plus
  --port PORT         port of the simulator page and API (default: 8090)
";

const DEFAULT_SIMULATOR_PORT: u16 = 8090;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let mut args = std::env::args().skip(1).peekable();
    if args.next_if(|arg| arg == "simulator").is_some() {
        return run_simulator(args);
    }

    let mut config_path = PathBuf::from("config.toml");
    let mut check = false;
    let mut preview: Option<PathBuf> = None;
    let mut simulator_url: Option<String> = None;
    let mut advertise_host: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--simulator" => {
                simulator_url = Some(args.next().context("--simulator needs a URL")?);
            }
            "--advertise-host" => {
                let host = args.next().context("--advertise-host needs a host")?;
                if host.is_empty() {
                    bail!("--advertise-host is empty; in Docker, start with `just sim`");
                }
                config::check_advertise_host(&host).context("invalid --advertise-host")?;
                advertise_host = Some(host);
            }
            "--preview" => {
                preview = Some(args.next().context("--preview needs a file name")?.into());
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            s if s.starts_with('-') => bail!("unknown option {s}\n\n{USAGE}"),
            s => config_path = s.into(),
        }
    }

    let mut cfg = Config::load(&config_path)?;
    if advertise_host.is_some() {
        cfg.advertise_host = advertise_host;
    }
    let albums = library::scan(&cfg.music_dir)?;
    info!(
        "found {} albums in {}",
        albums.len(),
        cfg.music_dir.display()
    );

    let base_url =
        advertise_address(&cfg).map(|host| format!("http://{host}:{}/music", cfg.http_port));

    if let Some(path) = preview {
        return write_preview(&cfg, albums, base_url?, &path);
    }
    if check {
        return run_check(&cfg, &albums, base_url.as_deref(), simulator_url.as_deref());
    }
    let base_url = base_url?;

    server::spawn(
        cfg.music_dir.clone(),
        cfg.http_port,
        library::served_files(&albums),
    )?;
    info!("serving music at {base_url}/");

    let (event_tx, event_rx) = mpsc::channel();
    let player = player::spawn(
        cfg.speaker_type,
        cfg.speaker_host.clone(),
        cfg.speaker_port(),
        event_tx,
    );
    let mut ui = Ui::new(&cfg, albums, base_url, player, event_rx);

    // Keep looking for a deck; survive it being unplugged and plugged back in.
    let mut source = match simulator_url {
        Some(url) => DeckSource::Simulator(url),
        None => DeckSource::Usb(elgato_streamdeck::new_hidapi()?),
    };
    let mut waiting_logged = false;
    loop {
        match source.open() {
            Ok(Some(mut deck)) => {
                info!("deck connected: {}", deck.name());
                waiting_logged = false;
                if let Err(err) = ui.run(&mut deck, cfg.brightness) {
                    warn!("deck disconnected: {err:#}");
                }
            }
            Ok(None) if !waiting_logged => {
                info!("{}", source.waiting_message());
                waiting_logged = true;
            }
            Ok(None) => {}
            Err(err) => warn!("cannot open the deck: {err:#}"),
        }
        ui.handle_events();
        std::thread::sleep(Duration::from_secs(2));
    }
}

enum DeckSource {
    Usb(HidApi),
    Simulator(String),
}

impl DeckSource {
    fn open(&mut self) -> Result<Option<Deck>> {
        match self {
            DeckSource::Usb(hid) => Deck::open_usb(hid),
            DeckSource::Simulator(url) => Deck::open_simulator(url),
        }
    }

    fn waiting_message(&self) -> String {
        match self {
            DeckSource::Usb(_) => "waiting for a Stream Deck to be plugged in…".into(),
            DeckSource::Simulator(url) => format!("waiting for the deck simulator at {url}…"),
        }
    }
}

fn run_simulator(mut args: impl Iterator<Item = String>) -> Result<()> {
    let mut model = Model::Mk2;
    let mut port = DEFAULT_SIMULATOR_PORT;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" => {
                let name = args.next().context("--model needs a name")?;
                model = Model::parse(&name).with_context(|| {
                    format!(
                        "unknown model {name}; use one of: {}",
                        Model::NAMES.join(", ")
                    )
                })?;
            }
            "--port" => {
                let value = args.next().context("--port needs a number")?;
                port = value
                    .parse()
                    .with_context(|| format!("--port needs a number, not {value}"))?;
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            s => bail!("unknown simulator option {s}\n\n{USAGE}"),
        }
    }
    simulator::run(port, model)
}

fn write_preview(
    cfg: &Config,
    albums: Vec<library::Album>,
    base_url: String,
    path: &Path,
) -> Result<()> {
    let (tx, _) = mpsc::channel();
    let (_, rx) = mpsc::channel();
    let mut ui = Ui::new(cfg, albums, base_url, tx, rx);
    ui.preview(3, 5, 144, true)
        .save(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    info!("wrote {}", path.display());
    Ok(())
}

fn advertise_address(cfg: &Config) -> Result<String> {
    match &cfg.advertise_host {
        Some(host) => Ok(host.clone()),
        None => local_ip_towards(&cfg.speaker_host, cfg.speaker_port()).with_context(|| {
            format!(
                "cannot find a route to speaker_host {} (port {}); check it, or set advertise_host",
                cfg.speaker_host,
                cfg.speaker_port()
            )
        }),
    }
}

/// The local address this machine uses to reach the speaker. No packets are
/// sent: connecting a UDP socket only picks the route.
fn local_ip_towards(host: &str, port: u16) -> Result<String> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect((host, port))?;
    Ok(socket.local_addr()?.ip().to_string())
}

fn run_check(
    cfg: &Config,
    albums: &[library::Album],
    base_url: Result<&str, &anyhow::Error>,
    simulator_url: Option<&str>,
) -> Result<()> {
    println!("Albums in {}:", cfg.music_dir.display());
    for album in albums {
        let cover = if album.cover.is_some() {
            "cover ✓"
        } else {
            "no cover"
        };
        println!(
            "  {:<40} {:>3} tracks, {cover}",
            album.name,
            album.tracks.len()
        );
    }
    match base_url {
        Ok(base_url) => {
            if let Some(track) = albums.first().and_then(|a| a.tracks.first()) {
                println!(
                    "\nExample URL the speaker will fetch:\n  {}",
                    library::url_for(base_url, &track.rel_path)
                );
            }
        }
        Err(err) => println!("\nThe speaker cannot fetch music:\n  {err:#}"),
    }

    if let Some(url) = simulator_url {
        println!("\nDeck simulator {url}:");
        match Deck::simulator_info(url) {
            Ok(Some(info)) => println!(
                "  reachable ✓  {}x{} keys of {} px",
                info.rows, info.cols, info.key_size
            ),
            Ok(None) => println!("  NOT reachable (start it with `kids-deck simulator`)"),
            Err(err) => println!("  NOT usable: {err:#}"),
        }
    } else {
        print_usb_decks()?;
    }

    println!(
        "\nSpeaker {}:{} ({:?}):",
        cfg.speaker_host,
        cfg.speaker_port(),
        cfg.speaker_type
    );
    match cfg.speaker_type {
        SpeakerType::Cast => print_cast_speaker(cfg),
        SpeakerType::Heos => print_heos_players(cfg),
    }
    Ok(())
}

fn print_cast_speaker(cfg: &Config) {
    let reachable = (cfg.speaker_host.as_str(), cfg.speaker_port())
        .to_socket_addrs()
        .map_err(anyhow::Error::from)
        .and_then(|mut addrs| addrs.next().context("cannot resolve speaker address"))
        .and_then(|addr| Ok(TcpStream::connect_timeout(&addr, Duration::from_secs(3))?));
    if let Err(err) = reachable {
        println!("  NOT reachable: {err:#}");
        return;
    }
    match rust_cast::CastDevice::connect_without_host_verification(
        cfg.speaker_host.clone(),
        cfg.speaker_port(),
    )
    .map_err(anyhow::Error::from)
    .and_then(|device| {
        device.connection.connect("receiver-0")?;
        Ok(device.receiver.get_status()?)
    }) {
        Ok(status) => {
            let volume = status
                .volume
                .level
                .map_or("?".into(), |v| format!("{:.0}%", v * 100.0));
            println!("  reachable ✓  volume {volume}");
            for app in status.applications {
                println!("  running app: {} ({})", app.display_name, app.app_id);
            }
        }
        Err(err) => println!("  NOT reachable: {err:#}"),
    }
}

fn print_heos_players(cfg: &Config) {
    match player::heos::players(&cfg.speaker_host, cfg.speaker_port()) {
        Ok(players) if players.is_empty() => println!("  reachable ✓  but it knows no players"),
        Ok(players) => {
            println!("  reachable ✓");
            for p in players {
                let ip = p.ip.as_deref().unwrap_or("?");
                println!("  player {} ({}), ip {ip}, pid {}", p.name, p.model, p.pid);
            }
        }
        Err(err) => println!("  NOT reachable: {err:#}"),
    }
}

fn print_usb_decks() -> Result<()> {
    println!("\nStream Decks:");
    let hid = elgato_streamdeck::new_hidapi()?;
    let decks = elgato_streamdeck::list_devices(&hid);
    if decks.is_empty() {
        println!("  none found (on macOS, quit the Elgato Stream Deck app first)");
    }
    for (kind, serial) in decks {
        println!(
            "  {kind:?} ({} keys, {}x{}), serial {serial}",
            kind.key_count(),
            kind.row_count(),
            kind.column_count()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(extra: &str) -> Config {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, format!("music_dir = \"music\"\n{extra}")).unwrap();
        Config::load(&path).unwrap()
    }

    #[test]
    fn no_route_to_the_speaker_names_speaker_host() {
        // An IPv4 socket cannot connect to an IPv6 address: fails without DNS or network.
        let err = advertise_address(&config("speaker_host = \"::1\"\n")).unwrap_err();
        assert!(err.to_string().contains("speaker_host ::1"), "{err:#}");
    }

    #[test]
    fn advertise_host_skips_detection() {
        let cfg = config("speaker_host = \"::1\"\nadvertise_host = \"10.0.0.2\"\n");
        assert_eq!(advertise_address(&cfg).unwrap(), "10.0.0.2");
    }
}
