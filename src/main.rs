//! kids-deck: a music player for kids.
//!
//! Album covers are shown on an Elgato Stream Deck. Pressing a cover plays
//! the album on a network speaker (Chromecast or HEOS) or on this computer's
//! sound output. The program serves the source folders over HTTP and tells
//! the speaker to fetch the tracks from it.
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
mod net;
mod player;
mod podcasts;
mod radio;
mod server;
mod simulator;
mod spotify;
mod state;
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
use crate::library::Library;
use crate::simulator::Model;
use crate::ui::Ui;

const USAGE: &str = "\
usage: kids-deck [CONFIG] [--simulator URL] [--advertise-host HOST]
                 [--check | --preview FILE.png]
       kids-deck simulator [--model NAME] [--port PORT]
       kids-deck spotify-login [CONFIG] [--listen ADDR]

  CONFIG              path to config.toml (default: ./config.toml)
  --simulator URL     use the deck simulator at URL, e.g. http://localhost:8090,
                      instead of a USB Stream Deck
  --advertise-host HOST
                      the address the speaker uses to reach this program;
                      overrides advertise_host in the config
  --check             list sources, Stream Decks and speaker status, then exit
  --preview FILE.png  draw the 15-key layout into a picture, then exit

  simulator           run the web Stream Deck simulator
  --model NAME        mk2 (default), mini, neo, xl or plus
  --port PORT         port of the simulator page and API (default: 8090)

  spotify-login       sign in to Spotify once; saves the login in state_dir
  --listen ADDR       where the browser comes back to (default: 127.0.0.1:8898)
";

const DEFAULT_SPOTIFY_LISTEN: &str = "127.0.0.1:8898";

const DEFAULT_SIMULATOR_PORT: u16 = 8090;

fn main() -> Result<()> {
    net::install_crypto();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                // ureq_proto logs raw requests, tokens included, at trace level.
                .unwrap_or_else(|_| EnvFilter::new("info,symphonia=error,ureq_proto=info")),
        )
        .init();

    let mut args = std::env::args().skip(1).peekable();
    if args.next_if(|arg| arg == "simulator").is_some() {
        return run_simulator(args);
    }
    if args.next_if(|arg| arg == "spotify-login").is_some() {
        return run_spotify_login(args);
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
    let library = scan_sources(&cfg);

    let base_url =
        advertise_address(&cfg).map(|host| format!("http://{host}:{}/music", cfg.http_port));

    if let Some(path) = preview {
        return write_preview(&cfg, library, base_url?, &path);
    }
    if check {
        return run_check(
            &cfg,
            &library,
            base_url.as_deref(),
            simulator_url.as_deref(),
        );
    }
    let base_url = base_url?;

    let served = server::Served::new(library.served_files());
    if cfg.speaker_type != SpeakerType::Local {
        serve_music(&cfg, served.clone())?;
        info!("serving music at {base_url}/");
    }

    let (event_tx, event_rx) = mpsc::channel();
    let player = player::spawn(output(&cfg), event_tx);
    let store = state::Store::open(&cfg.state_dir);
    let mut ui = Ui::new(&cfg, library, base_url, player, event_rx, store);
    ui.set_podcasts(start_podcasts(&cfg, &served));

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
        ui.take_snapshots();
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn scan_sources(cfg: &Config) -> Library {
    let library = Library::scan(&cfg.sources);
    for (source, shelf) in cfg.sources.iter().zip(library.shelves()) {
        info!(
            "found {} items in {} ({})",
            shelf.items.len(),
            source.name,
            source.path.display()
        );
    }
    library
}

/// One `podcasts` thread per podcast source; a source that cannot start
/// keeps the episodes already in its cache.
fn start_podcasts(cfg: &Config, served: &server::Served) -> Vec<ui::PodcastThread> {
    cfg.sources
        .iter()
        .enumerate()
        .filter_map(|(shelf, source)| {
            let settings = library::podcast::settings(source)?;
            let (snapshot_tx, snapshots) = mpsc::channel();
            let files = server::PodcastFiles {
                source: source.name.clone(),
                served: served.clone(),
            };
            match podcasts::spawn(
                settings.clone(),
                settings.timing(),
                Box::new(files),
                snapshot_tx,
            ) {
                Ok(now_playing) => Some(ui::PodcastThread::new(
                    shelf,
                    source.clone(),
                    snapshots,
                    now_playing,
                )),
                Err(err) => {
                    warn!("podcast source {}: {err:#}", source.name);
                    None
                }
            }
        })
        .collect()
}

fn serve_music(cfg: &Config, served: server::Served) -> Result<()> {
    let roots = cfg
        .sources
        .iter()
        .filter(|s| s.serves_files())
        .map(|s| (s.name.clone(), s.path.clone()))
        .collect();
    server::spawn(roots, cfg.http_port, served)
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

fn run_spotify_login(args: impl Iterator<Item = String>) -> Result<()> {
    let mut config_path = PathBuf::from("config.toml");
    let mut listen = DEFAULT_SPOTIFY_LISTEN.to_string();
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen = args.next().context("--listen needs an address")?,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            s if s.starts_with('-') => bail!("unknown spotify-login option {s}\n\n{USAGE}"),
            s => config_path = s.into(),
        }
    }
    let listen = listen
        .parse()
        .with_context(|| format!("--listen needs an address like 127.0.0.1:8898, not {listen}"))?;
    let cfg = Config::load(&config_path)?;
    let spotify = cfg.spotify.as_ref().with_context(|| {
        format!(
            "{} has no [spotify] table with the client_id of your Spotify app",
            config_path.display()
        )
    })?;
    spotify::login::run(
        &spotify.client_id,
        &cfg.state_dir,
        listen,
        std::io::BufReader::new(std::io::stdin()),
        spotify::api::Endpoints::default(),
        &mut std::io::stdout(),
    )
}

fn write_preview(cfg: &Config, library: Library, base_url: String, path: &Path) -> Result<()> {
    let (tx, _) = mpsc::channel();
    let (_, rx) = mpsc::channel();
    let mut ui = Ui::new(cfg, library, base_url, tx, rx, state::Store::in_memory());
    ui.preview(3, 5, 144, true)
        .save(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    info!("wrote {}", path.display());
    Ok(())
}

fn output(cfg: &Config) -> player::Output {
    let (host, port) = (cfg.speaker_host.clone(), cfg.speaker_port());
    match cfg.speaker_type {
        SpeakerType::Cast => player::Output::Cast { host, port },
        SpeakerType::Heos => player::Output::Heos { host, port },
        SpeakerType::Local => player::Output::Local {
            device: cfg.audio_device.clone(),
        },
    }
}

fn advertise_address(cfg: &Config) -> Result<String> {
    if cfg.speaker_type == SpeakerType::Local {
        // Nothing downloads from us: the music URLs are never used.
        return Ok("127.0.0.1".into());
    }
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
    library: &Library,
    base_url: Result<&str, &anyhow::Error>,
    simulator_url: Option<&str>,
) -> Result<()> {
    for (source, shelf) in cfg.sources.iter().zip(library.shelves()) {
        println!(
            "Source {} ({:?}) in {}:",
            source.name,
            source.kind,
            source.path.display()
        );
        if let Some(podcast) = &source.podcast {
            for feed in &podcast.feeds {
                match podcasts::check_feed(&feed.url) {
                    Ok((title, episodes)) => {
                        println!("  feed {}: {title:?}, {episodes} episodes ✓", feed.name);
                    }
                    Err(err) => println!("  feed {}: NOT readable: {err:#}", feed.name),
                }
            }
        }
        if shelf.items.is_empty() {
            println!("  nothing to play");
        }
        for &id in &shelf.items {
            let item = library.item(id);
            let cover = if item.cover.is_some() {
                "cover ✓"
            } else {
                "no cover"
            };
            println!(
                "  {:<40} {:>3} tracks, {cover}",
                item.name,
                item.tracks().len()
            );
        }
    }
    match base_url {
        Ok(base_url) => {
            if let Some(track) = library.items().next().and_then(|(_, a)| a.tracks().first()) {
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

    if let Some(spotify) = &cfg.spotify {
        print_spotify(cfg, spotify);
    }
    if cfg.speaker_type == SpeakerType::Local {
        print_audio_outputs(cfg);
        return Ok(());
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
        SpeakerType::Local => unreachable!("handled above"),
    }
    Ok(())
}

fn print_spotify(cfg: &Config, spotify: &config::Spotify) {
    println!("\nSpotify:");
    let mut client =
        match spotify::api::Client::load(&cfg.state_dir, spotify::api::Endpoints::default()) {
            Ok(client) => client,
            Err(err) => {
                println!("  {err:#}");
                return;
            }
        };
    let mut out = Vec::new();
    if let Err(err) = spotify::login::print_account(&mut client, &mut out) {
        println!("  {err:#}");
    }
    for line in String::from_utf8_lossy(&out).lines() {
        println!("  {line}");
    }
    let Some(device) = &spotify.device else {
        println!("  no spotify.device set: Spotify cannot play yet");
        return;
    };
    match client.devices() {
        Ok(devices) => match spotify::pick_device(&devices, device) {
            Ok(found) => println!("  spotify.device {device:?} is {:?} ✓", found.name),
            Err(err) => println!("  spotify.device {device:?}: {err:#}"),
        },
        Err(err) => println!("  cannot check spotify.device: {err:#}"),
    }
}

fn print_audio_outputs(cfg: &Config) {
    let wanted = cfg.audio_device.as_deref().unwrap_or("the default");
    println!("\nAudio out (local), using {wanted}:");
    match player::local::output_devices() {
        Ok(names) if names.is_empty() => println!("  no sound output found"),
        Ok(names) => names.iter().for_each(|name| println!("  {name}")),
        Err(err) => println!("  cannot list sound outputs: {err:#}"),
    }
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
        let music = "[[source]]\ntype = \"music\"\npath = \"music\"\n";
        std::fs::write(&path, format!("{extra}{music}")).unwrap();
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
