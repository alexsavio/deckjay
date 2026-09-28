//! deckjay: a music player with no screen, driven by pictures on a Stream Deck.
//!
//! Album covers are shown on an Elgato Stream Deck. Pressing a cover plays
//! the album on a network speaker (Chromecast or HEOS), on a Spotify Connect
//! device, or on this computer's sound output. The program serves the source
//! folders over HTTP and tells the speaker to fetch the tracks from it.
//!
//! Three threads work together:
//!
//! - main: finds the deck, draws the keys and reacts to key presses
//!   ([`ui::Ui`], [`deck::Deck`]).
//! - `cast` or `heos`: sends commands to the speaker and reports its state
//!   ([`player::spawn_with`]).
//! - `http`: serves the music files to the speaker ([`server::spawn`]).
//!
//! `deckjay simulator` runs something else: [`simulator`], a web page that
//! stands in for the Stream Deck, so the player can run without the hardware.
//! The other commands ([`cli`]) check the setup or the deck and exit.

mod cli;
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
mod systemd;
mod ui;

use std::fmt::Display;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::{Context, Result};
use hidapi::HidApi;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::cli::{Command, RunArgs};
use crate::config::{Config, SpeakerType};
use crate::deck::Deck;
use crate::library::Library;
use crate::net::BaseUrl;
use crate::ui::Ui;

fn main() -> Result<ExitCode> {
    net::install_crypto();
    tracing_subscriber::fmt()
        .with_ansi(colour())
        .with_env_filter(
            EnvFilter::try_from_default_env()
                // ureq_proto logs raw requests, tokens included, at trace level.
                .unwrap_or_else(|_| EnvFilter::new("info,symphonia=error,ureq_proto=info")),
        )
        .init();

    match cli::parse() {
        Command::Run(args) => run_player(&args)?,
        Command::Check(args) => return run_check(&args),
        Command::CheckConfig { config } => check_config(&config)?,
        Command::Preview { file, config } => {
            let cfg = Config::load(&config)?;
            let library = scan_sources(&cfg);
            write_preview(&cfg, library, base_url(&cfg), &file)?;
        }
        Command::Blank => blank_usb_deck()?,
        Command::Simulator { model, port } => simulator::run(port, model)?,
        Command::SpotifyLogin { config, listen } => run_spotify_login(&config, listen)?,
    }
    Ok(ExitCode::SUCCESS)
}

/// No colour codes when `NO_COLOR` is set (no-color.org) or when the output
/// goes to systemd's journal, which sets `JOURNAL_STREAM`. `with_ansi`
/// replaces tracing's own `NO_COLOR` default, so both are read here.
fn colour() -> bool {
    std::env::var_os("JOURNAL_STREAM").is_none()
        && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
}

fn load_config(args: &RunArgs) -> Result<Config> {
    let mut cfg = Config::load(&args.config)?;
    if args.advertise_host.is_some() {
        cfg.advertise_host.clone_from(&args.advertise_host);
    }
    Ok(cfg)
}

fn run_player(args: &RunArgs) -> Result<()> {
    let cfg = load_config(args)?;
    let library = scan_sources(&cfg);
    let mut base_url = base_url(&cfg);

    let served = server::Served::new(library.served_files());
    if cfg.speaker_type != SpeakerType::Local {
        serve_music(&cfg, served.clone())?;
        match base_url.try_get() {
            Ok(url) => info!("serving music at {url}/"),
            Err(err) => warn!("{err:#}; the deck starts anyway and looks again at each album"),
        }
    }

    let (event_tx, event_rx) = mpsc::channel();
    let player = player::spawn_with(output(&cfg), spotify_output(&cfg), event_tx);
    let store = state::Store::open(&cfg.state_dir);
    let mut ui = Ui::new(&cfg, library, base_url, player, event_rx, store);
    ui.set_podcasts(start_podcasts(&cfg, &served));
    ui.set_watchdog(systemd::Watchdog::from_env());
    fetch_playlist_covers(&cfg);

    let stop = stop_on_signals()?;
    let source = match &args.simulator {
        Some(url) => DeckSource::Simulator(url.clone()),
        None => DeckSource::Usb(elgato_streamdeck::new_hidapi()?),
    };
    // Ready without a deck: the program waits for one.
    systemd::ready();
    let outcome = drive_decks(&mut ui, source, cfg.brightness, &stop);
    info!("stopping");
    ui.save_state();
    systemd::stopping();
    outcome
}

/// For the scripts that write the config (Ansible's `validate:`): reads the
/// file and nothing else.
fn check_config(path: &Path) -> Result<()> {
    let cfg = Config::load(path)?;
    let names: Vec<&str> = cfg.sources.iter().map(|s| s.name.as_str()).collect();
    println!(
        "{} is valid: {:?} speaker, {} ({})",
        path.display(),
        cfg.speaker_type,
        count(names.len(), "source"),
        names.join(", ")
    );
    Ok(())
}

/// Ctrl-C, closing the terminal, `systemctl stop` and `docker stop` set the
/// flag, which turns the deck dark; a second signal ends the program at once.
fn stop_on_signals() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register_conditional_shutdown(signal, 1, Arc::clone(&stop))?;
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }
    Ok(stop)
}

/// Keeps looking for a deck, and survives it being unplugged and plugged
/// back in, until `stop` is set. Returns [`ui::PlayerGone`] when the player
/// thread ended: the program exits with it, so the service restarts.
fn drive_decks(
    ui: &mut Ui,
    mut source: DeckSource,
    brightness: u8,
    stop: &AtomicBool,
) -> Result<()> {
    let mut waiting_logged = false;
    while !stop.load(Ordering::SeqCst) {
        match source.open() {
            Ok(Some(mut deck)) => {
                info!("deck connected: {}", deck.name());
                waiting_logged = false;
                match ui.run(&mut deck, brightness, stop) {
                    Ok(()) => {}
                    Err(err) if err.is::<ui::PlayerGone>() => return Err(err),
                    Err(err) => warn!("deck disconnected: {err:#}"),
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
        if ui.player_gone() {
            return Err(ui::PlayerGone.into());
        }
        for _ in 0..20 {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            ui.heartbeat();
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

/// For systemd's `ExecStopPost`, which runs however deckjay ended, a crash
/// included.
fn blank_usb_deck() -> Result<()> {
    let mut hid = elgato_streamdeck::new_hidapi()?;
    if let Some(mut deck) = Deck::open_usb_as_is(&mut hid)? {
        deck.blank()?;
        info!("{} is dark", deck.name());
    } else {
        info!("no Stream Deck to turn off");
    }
    Ok(())
}

fn scan_sources(cfg: &Config) -> Library {
    let library = Library::scan(&cfg.sources);
    for (source, shelf) in cfg.sources.iter().zip(library.shelves()) {
        let count = shelf.items.len();
        if source.serves_files() {
            info!(
                "found {count} items in {} ({})",
                source.name,
                source.path.display()
            );
        } else {
            info!("found {count} items in {}", source.name);
        }
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

fn run_spotify_login(config_path: &Path, listen: SocketAddr) -> Result<()> {
    let cfg = Config::load(config_path)?;
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

fn write_preview(cfg: &Config, library: Library, base_url: BaseUrl, path: &Path) -> Result<()> {
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

/// In the background: new covers show from the next start.
fn fetch_playlist_covers(cfg: &Config) {
    for source in cfg.sources.iter().filter(|s| !s.playlists.is_empty()) {
        let uris: Vec<String> = source.playlists.iter().map(|p| p.uri.clone()).collect();
        let (state_dir, folder) = (cfg.state_dir.clone(), source.path.clone());
        let fetch = move || {
            spotify::covers::fetch_missing(
                &state_dir,
                &folder,
                &uris,
                spotify::api::Endpoints::default(),
            );
        };
        if let Err(err) = std::thread::Builder::new()
            .name("covers".into())
            .spawn(fetch)
        {
            warn!("cannot fetch playlist covers: {err}");
        }
    }
}

/// Spotify plays only with a device to play on.
fn spotify_output(cfg: &Config) -> Option<player::spotify::Connect> {
    let device = cfg.spotify.as_ref()?.device.clone()?;
    Some(player::spotify::Connect {
        state_dir: cfg.state_dir.clone(),
        device,
    })
}

fn base_url(cfg: &Config) -> BaseUrl {
    if cfg.speaker_type == SpeakerType::Local {
        // Nothing downloads from us: the music URLs are never used.
        return BaseUrl::fixed(format!("http://127.0.0.1:{}/music", cfg.http_port));
    }
    match &cfg.advertise_host {
        Some(host) => BaseUrl::fixed(format!("http://{host}:{}/music", cfg.http_port)),
        None => BaseUrl::towards(&cfg.speaker_host, cfg.speaker_port(), cfg.http_port),
    }
}

/// What `check` found, for the exit code: 0 all good, 1 problems, 2 warnings
/// only. A problem means the deck cannot play something; a warning means it
/// plays, but something is missing.
#[derive(Default)]
struct Report {
    problems: usize,
    warnings: usize,
}

impl Report {
    fn problem(&mut self, line: impl Display) {
        self.problems += 1;
        println!("{line}");
    }

    fn warning(&mut self, line: impl Display) {
        self.warnings += 1;
        println!("{line}");
    }

    fn code(&self) -> u8 {
        match (self.problems, self.warnings) {
            (0, 0) => 0,
            (0, _) => 2,
            _ => 1,
        }
    }

    /// Prints the totals and gives the exit code.
    fn finish(self) -> ExitCode {
        if self.code() == 0 {
            println!("\nAll good ✓");
        } else {
            println!(
                "\n{}, {}",
                count(self.problems, "problem"),
                count(self.warnings, "warning")
            );
        }
        ExitCode::from(self.code())
    }
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn run_check(args: &RunArgs) -> Result<ExitCode> {
    let cfg = load_config(args)?;
    let library = scan_sources(&cfg);
    let mut base_url = base_url(&cfg);
    let mut report = Report::default();
    for (source, shelf) in cfg.sources.iter().zip(library.shelves()) {
        if source.serves_files() {
            let (name, kind, path) = (&source.name, source.kind, source.path.display());
            println!("Source {name} ({kind:?}) in {path}:");
        } else {
            println!("Source {} ({:?}):", source.name, source.kind);
        }
        if let Some(podcast) = &source.podcast {
            for feed in &podcast.feeds {
                match podcasts::check_feed(&feed.url) {
                    Ok((title, episodes)) => {
                        println!("  feed {}: {title:?}, {episodes} episodes ✓", feed.name);
                    }
                    Err(err) => {
                        report.problem(format!("  feed {}: NOT readable: {err:#}", feed.name));
                    }
                }
            }
        }
        if shelf.items.is_empty() {
            report.problem("  nothing to play");
        }
        for &id in &shelf.items {
            let item = library.item(id);
            let cover = if item.cover.is_some() {
                "cover ✓"
            } else {
                "no cover"
            };
            let (line, fine) = match &item.media {
                library::Media::Tracks(tracks) => (
                    format!("  {:<40} {:>3} tracks, {cover}", item.name, tracks.len()),
                    item.cover.is_some(),
                ),
                // Stations show a glyph, never a cover.
                library::Media::Stream { url } => (format!("  {:<40} {url}", item.name), true),
                library::Media::Spotify { uri } => (
                    format!("  {:<40} {uri}, {cover}", item.name),
                    item.cover.is_some(),
                ),
            };
            if fine {
                println!("{line}");
            } else {
                report.warning(line);
            }
        }
    }
    match base_url.try_get() {
        Ok(base_url) => {
            if let Some(track) = library.items().next().and_then(|(_, a)| a.tracks().first()) {
                println!(
                    "\nExample URL the speaker will fetch:\n  {}",
                    library::url_for(&base_url, &track.rel_path)
                );
            }
        }
        Err(err) => report.problem(format!("\nThe speaker cannot fetch music:\n  {err:#}")),
    }

    if let Some(url) = &args.simulator {
        println!("\nDeck simulator {url}:");
        match Deck::simulator_info(url) {
            Ok(Some(info)) => println!(
                "  reachable ✓  {}x{} keys of {} px",
                info.rows, info.cols, info.key_size
            ),
            Ok(None) => report.problem("  NOT reachable (start it with `deckjay simulator`)"),
            Err(err) => report.problem(format!("  NOT usable: {err:#}")),
        }
    } else {
        print_usb_decks(&mut report);
    }

    if let Some(spotify) = &cfg.spotify {
        print_spotify(&cfg, spotify, &mut report);
    }
    if cfg.speaker_type == SpeakerType::Local {
        print_audio_outputs(&cfg, &mut report);
        return Ok(report.finish());
    }
    println!(
        "\nSpeaker {}:{} ({:?}):",
        cfg.speaker_host,
        cfg.speaker_port(),
        cfg.speaker_type
    );
    match cfg.speaker_type {
        SpeakerType::Cast => print_cast_speaker(&cfg, &mut report),
        SpeakerType::Heos => print_heos_players(&cfg, &mut report),
        SpeakerType::Local => unreachable!("handled above"),
    }
    Ok(report.finish())
}

fn print_spotify(cfg: &Config, spotify: &config::Spotify, report: &mut Report) {
    println!("\nSpotify:");
    let mut client =
        match spotify::api::Client::load(&cfg.state_dir, spotify::api::Endpoints::default()) {
            Ok(client) => client,
            Err(err) => {
                report.problem(format!("  {err:#}"));
                return;
            }
        };
    let mut out = Vec::new();
    if let Err(err) = spotify::login::print_account(&mut client, &mut out) {
        report.problem(format!("  {err:#}"));
    }
    for line in String::from_utf8_lossy(&out).lines() {
        println!("  {line}");
    }
    let Some(device) = &spotify.device else {
        report.warning("  no spotify.device set: Spotify cannot play yet");
        return;
    };
    match client.devices() {
        Ok(devices) => match spotify::pick_device(&devices, device) {
            Ok(found) => println!("  spotify.device {device:?} is {:?} ✓", found.name),
            Err(err) => report.problem(format!("  spotify.device {device:?}: {err:#}")),
        },
        Err(err) => report.problem(format!("  cannot check spotify.device: {err:#}")),
    }
}

fn print_audio_outputs(cfg: &Config, report: &mut Report) {
    let wanted = cfg.audio_device.as_deref().unwrap_or("the default");
    println!("\nAudio out (local), using {wanted}:");
    match player::local::output_devices() {
        Ok(names) if names.is_empty() => report.problem("  no sound output found"),
        Ok(names) => names.iter().for_each(|name| println!("  {name}")),
        Err(err) => report.problem(format!("  cannot list sound outputs: {err:#}")),
    }
}

fn print_cast_speaker(cfg: &Config, report: &mut Report) {
    let reachable = (cfg.speaker_host.as_str(), cfg.speaker_port())
        .to_socket_addrs()
        .map_err(anyhow::Error::from)
        .and_then(|mut addrs| addrs.next().context("cannot resolve speaker address"))
        .and_then(|addr| Ok(TcpStream::connect_timeout(&addr, Duration::from_secs(3))?));
    if let Err(err) = reachable {
        report.problem(format!("  NOT reachable: {err:#}"));
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
        Err(err) => report.problem(format!("  NOT reachable: {err:#}")),
    }
}

fn print_heos_players(cfg: &Config, report: &mut Report) {
    match player::heos::players(&cfg.speaker_host, cfg.speaker_port()) {
        Ok(players) if players.is_empty() => {
            report.problem("  reachable ✓  but it knows no players");
        }
        Ok(players) => {
            println!("  reachable ✓");
            for p in players {
                let ip = p.ip.as_deref().unwrap_or("?");
                println!("  player {} ({}), ip {ip}, pid {}", p.name, p.model, p.pid);
            }
        }
        Err(err) => report.problem(format!("  NOT reachable: {err:#}")),
    }
}

/// A failure to list USB devices is one line of the report, not its end.
fn print_usb_decks(report: &mut Report) {
    println!("\nStream Decks:");
    let hid = match elgato_streamdeck::new_hidapi() {
        Ok(hid) => hid,
        Err(err) => {
            report.problem(format!("  cannot list USB devices: {err}"));
            return;
        }
    };
    let decks = elgato_streamdeck::list_devices(&hid);
    if decks.is_empty() {
        // The player waits for a deck, so a missing one is not a problem.
        report.warning("  none found (on macOS, quit the Elgato Stream Deck app first)");
    }
    for (kind, serial) in decks {
        println!(
            "  {kind:?} ({} keys, {}x{}), serial {serial}",
            kind.key_count(),
            kind.row_count(),
            kind.column_count()
        );
    }
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
        let err = base_url(&config("speaker_host = \"::1\"\n"))
            .try_get()
            .unwrap_err();
        assert!(err.to_string().contains("speaker_host ::1"), "{err:#}");
    }

    #[test]
    fn advertise_host_skips_detection() {
        let cfg = config("speaker_host = \"::1\"\nadvertise_host = \"10.0.0.2\"\n");
        assert_eq!(
            base_url(&cfg).try_get().unwrap(),
            format!("http://10.0.0.2:{}/music", cfg.http_port)
        );
    }

    #[test]
    fn local_audio_needs_no_route_to_a_speaker() {
        let cfg = config("speaker_type = \"local\"\n");
        assert!(base_url(&cfg).try_get().is_ok());
    }

    #[test]
    fn check_exit_code_is_1_for_problems_2_for_warnings_only() {
        let mut report = Report::default();
        assert_eq!(report.code(), 0);
        report.warning("an item with no cover");
        assert_eq!(report.code(), 2);
        report.problem("a speaker that does not answer");
        assert_eq!(report.code(), 1);
    }

    #[test]
    fn counts_read_well() {
        assert_eq!(count(1, "problem"), "1 problem");
        assert_eq!(count(0, "warning"), "0 warnings");
        assert_eq!(count(3, "warning"), "3 warnings");
    }
}
