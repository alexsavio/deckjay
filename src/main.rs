//! kids-deck: a music player for kids.
//!
//! Album covers are shown on an Elgato Stream Deck. Pressing a cover plays
//! the album on a Chromecast speaker. The program serves the music folder
//! over HTTP and tells the speaker to fetch the tracks from it.
//!
//! Three threads work together:
//!
//! - main: finds the deck, draws the keys and reacts to key presses
//!   ([`ui::Ui`], [`deck::Deck`]).
//! - `cast`: sends commands to the speaker and reports its state
//!   ([`player::spawn`]).
//! - `http`: serves the music files to the speaker ([`server::spawn`]).

mod config;
mod deck;
mod icons;
mod library;
mod player;
mod server;
mod ui;

use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::deck::Deck;
use crate::ui::Ui;

const USAGE: &str = "\
usage: kids-deck [CONFIG] [--check | --preview FILE.png]

  CONFIG              path to config.toml (default: ./config.toml)
  --check             list albums, Stream Decks and speaker status, then exit
  --preview FILE.png  draw the 15-key layout into a picture, then exit
";

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let mut config_path = PathBuf::from("config.toml");
    let mut check = false;
    let mut preview: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
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

    let cfg = Config::load(&config_path)?;
    let albums = library::scan(&cfg.music_dir)?;
    info!(
        "found {} albums in {}",
        albums.len(),
        cfg.music_dir.display()
    );

    let advertise = match &cfg.advertise_host {
        Some(host) => host.clone(),
        None => local_ip_towards(&cfg.speaker_host, cfg.speaker_port)
            .context("cannot detect this machine's IP address; set advertise_host in the config")?,
    };
    let base_url = format!("http://{advertise}:{}/music", cfg.http_port);

    if let Some(path) = preview {
        let (tx, _) = mpsc::channel();
        let (_, rx) = mpsc::channel();
        let mut ui = Ui::new(&cfg, albums, base_url, tx, rx);
        ui.preview(3, 5, 144, true).save(&path)?;
        info!("wrote {}", path.display());
        return Ok(());
    }
    if check {
        return run_check(&cfg, &albums, &base_url);
    }

    server::spawn(cfg.music_dir.clone(), cfg.http_port)?;
    info!("serving music at {base_url}/");

    let (event_tx, event_rx) = mpsc::channel();
    let player = player::spawn(cfg.speaker_host.clone(), cfg.speaker_port, event_tx);
    let mut ui = Ui::new(&cfg, albums, base_url, player, event_rx);

    // Keep looking for a deck; survive it being unplugged and plugged back in.
    let mut hid = elgato_streamdeck::new_hidapi()?;
    let mut waiting_logged = false;
    loop {
        match Deck::open(&mut hid) {
            Ok(Some(mut deck)) => {
                info!("Stream Deck connected: {:?}", deck.kind());
                waiting_logged = false;
                if let Err(err) = ui.run(&mut deck, cfg.brightness) {
                    warn!("Stream Deck disconnected: {err:#}");
                }
            }
            Ok(None) if !waiting_logged => {
                info!("waiting for a Stream Deck to be plugged in…");
                waiting_logged = true;
            }
            Ok(None) => {}
            Err(err) => warn!("cannot open Stream Deck: {err:#}"),
        }
        ui.handle_events();
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// The local address this machine uses to reach the speaker. No packets are
/// sent: connecting a UDP socket only picks the route.
fn local_ip_towards(host: &str, port: u16) -> Result<String> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect((host, port))?;
    Ok(socket.local_addr()?.ip().to_string())
}

fn run_check(cfg: &Config, albums: &[library::Album], base_url: &str) -> Result<()> {
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
    if let Some(track) = albums.first().and_then(|a| a.tracks.first()) {
        println!(
            "\nExample URL the speaker will fetch:\n  {}",
            library::url_for(base_url, &track.rel_path)
        );
    }

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

    println!("\nSpeaker {}:{}:", cfg.speaker_host, cfg.speaker_port);
    let reachable = (cfg.speaker_host.as_str(), cfg.speaker_port)
        .to_socket_addrs()
        .map_err(anyhow::Error::from)
        .and_then(|mut addrs| addrs.next().context("cannot resolve speaker address"))
        .and_then(|addr| Ok(TcpStream::connect_timeout(&addr, Duration::from_secs(3))?));
    if let Err(err) = reachable {
        println!("  NOT reachable: {err:#}");
        return Ok(());
    }
    match rust_cast::CastDevice::connect_without_host_verification(
        cfg.speaker_host.clone(),
        cfg.speaker_port,
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
    Ok(())
}
