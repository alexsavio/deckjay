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
mod fsutil;
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
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::{Context, Result};
use hidapi::HidApi;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::cli::{Command, RunArgs};
use crate::config::{Config, Podcast, Source, SourceBody, SpeakerType};
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
    if cfg.speaker_type.serves_music() {
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
        if let Some(root) = source.served_root() {
            info!(
                "found {count} items in {} ({})",
                source.name,
                root.display()
            );
        } else {
            info!("found {count} items in {}", source.name);
        }
    }
    library
}

/// A podcast source to start a `podcasts` thread for.
struct PodcastStart<'a> {
    shelf: usize,
    source: &'a Source,
    settings: &'a Podcast,
    cache: &'a Path,
}

/// The podcast sources, each with its shelf index (its place in `sources`).
fn podcast_starts(sources: &[Source]) -> Vec<PodcastStart<'_>> {
    sources
        .iter()
        .enumerate()
        .filter_map(|(shelf, source)| {
            let SourceBody::Podcast { settings, cache } = &source.body else {
                return None;
            };
            Some(PodcastStart {
                shelf,
                source,
                settings,
                cache,
            })
        })
        .collect()
}

/// One `podcasts` thread per podcast source; a source that cannot start
/// keeps the episodes already in its cache.
fn start_podcasts(cfg: &Config, served: &server::Served) -> Vec<ui::PodcastThread> {
    podcast_starts(&cfg.sources)
        .into_iter()
        .filter_map(|start| {
            let PodcastStart {
                shelf,
                source,
                settings,
                cache,
            } = start;
            let (snapshot_tx, snapshots) = mpsc::channel();
            let files = server::PodcastFiles {
                source: source.name.clone(),
                served: served.clone(),
            };
            match podcasts::start(settings, cache, Box::new(files), snapshot_tx) {
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

/// The folder each source serves, by source name.
fn served_roots(sources: &[Source]) -> Vec<(String, PathBuf)> {
    sources
        .iter()
        .filter_map(|s| Some((s.name.clone(), s.served_root()?.to_path_buf())))
        .collect()
}

fn serve_music(cfg: &Config, served: server::Served) -> Result<()> {
    server::spawn(served_roots(&cfg.sources), cfg.http_port, served)
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

/// The playlist covers of one Spotify source to fetch.
struct CoverFetch<'a> {
    folder: &'a Path,
    uris: Vec<String>,
}

/// The Spotify sources with at least one playlist.
fn cover_fetches(sources: &[Source]) -> Vec<CoverFetch<'_>> {
    sources
        .iter()
        .filter_map(|source| {
            let SourceBody::Spotify { covers, playlists } = &source.body else {
                return None;
            };
            if playlists.is_empty() {
                return None;
            }
            Some(CoverFetch {
                folder: covers,
                uris: playlists.iter().map(|p| p.uri.clone()).collect(),
            })
        })
        .collect()
}

/// In the background: new covers show from the next start.
fn fetch_playlist_covers(cfg: &Config) {
    for CoverFetch { folder, uris } in cover_fetches(&cfg.sources) {
        let (state_dir, folder) = (cfg.state_dir.clone(), folder.to_path_buf());
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
    if !cfg.speaker_type.serves_music() {
        // Nothing downloads from us: the music URLs are never used.
        return BaseUrl::fixed(format!("http://127.0.0.1:{}/music", cfg.http_port));
    }
    match &cfg.advertise_host {
        Some(host) => BaseUrl::fixed(format!("http://{host}:{}/music", cfg.http_port)),
        None => BaseUrl::towards(&cfg.speaker_host, cfg.speaker_port(), cfg.http_port),
    }
}

/// What `check` found, for the exit code: 0 all good, 1 problems, 2 warnings
/// only. A problem means the deck cannot play something. A warning means it
/// plays, or will once something the player waits for arrives (a deck, a
/// simulator, the first podcast refresh), but something is missing.
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

/// The settings of a podcast source: only those have feeds to check.
fn podcast_settings(source: &Source) -> Option<&Podcast> {
    match &source.body {
        SourceBody::Podcast { settings, .. } => Some(settings),
        _ => None,
    }
}

fn run_check(args: &RunArgs) -> Result<ExitCode> {
    let cfg = load_config(args)?;
    let library = scan_sources(&cfg);
    let mut base_url = base_url(&cfg);
    let mut report = Report::default();
    for (source, shelf) in cfg.sources.iter().zip(library.shelves()) {
        if let Some(root) = source.served_root() {
            let (name, kind, path) = (&source.name, source.kind(), root.display());
            println!("Source {name} ({kind:?}) in {path}:");
        } else {
            println!("Source {} ({:?}):", source.name, source.kind());
        }
        let podcast = podcast_settings(source);
        if let Some(podcast) = podcast {
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
            if podcast.is_some() {
                // The episodes come with the first refresh, after the start.
                report.warning("  nothing to play yet: the episodes come with the first refresh");
            } else {
                report.problem("  nothing to play");
            }
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
            // The player waits for the simulator, as it waits for a USB deck.
            Ok(None) => report.warning("  NOT reachable (start it with `deckjay simulator`)"),
            Err(err) => report.problem(format!("  NOT usable: {err:#}")),
        }
    } else {
        print_usb_decks(&mut report);
    }

    if let Some(spotify) = &cfg.spotify {
        print_spotify(&cfg, spotify, &mut report);
    }
    match cfg.speaker_type {
        SpeakerType::Local => print_audio_outputs(&cfg, &mut report),
        SpeakerType::Cast => {
            print_speaker_header(&cfg);
            print_cast_speaker(&cfg, &mut report);
        }
        SpeakerType::Heos => {
            print_speaker_header(&cfg);
            print_heos_players(&cfg, &mut report);
        }
    }
    Ok(report.finish())
}

fn print_speaker_header(cfg: &Config) {
    println!(
        "\nSpeaker {}:{} ({:?}):",
        cfg.speaker_host,
        cfg.speaker_port(),
        cfg.speaker_type
    );
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
    match player::cast::probe(&cfg.speaker_host, cfg.speaker_port()) {
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
        Err(err) => report.problem(format!("  {err:#}")),
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
    report_usb_decks(&elgato_streamdeck::list_devices(&hid), report);
}

fn report_usb_decks(decks: &[(elgato_streamdeck::info::Kind, String)], report: &mut Report) {
    if decks.is_empty() {
        // The player waits for a deck, so a missing one is not a problem.
        report.warning("  none found (on macOS, quit the Elgato Stream Deck app first)");
    } else if !decks.iter().any(|&(kind, _)| deck::usb_usable(kind)) {
        report
            .warning("  none the player can use (it skips a model without a key grid it can draw)");
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
    fn a_usb_deck_the_player_would_skip_is_a_warning() {
        use elgato_streamdeck::info::Kind;
        let mut report = Report::default();
        report_usb_decks(&[(Kind::Pedal, "A1".to_owned())], &mut report);
        assert_eq!(report.warnings, 1);
        let mut report = Report::default();
        report_usb_decks(&[(Kind::Mk2, "A2".to_owned())], &mut report);
        assert_eq!(report.warnings, 0);
        let mut report = Report::default();
        let both = [(Kind::Pedal, "A1".to_owned()), (Kind::Mk2, "A2".to_owned())];
        report_usb_decks(&both, &mut report);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn counts_read_well() {
        assert_eq!(count(1, "problem"), "1 problem");
        assert_eq!(count(0, "warning"), "0 warnings");
        assert_eq!(count(3, "warning"), "3 warnings");
    }

    /// Music, podcast, radio, a second podcast and Spotify, in this order.
    fn mixed() -> Config {
        let feed = |name: &str| {
            format!(
                "[[source.feed]]\nname = \"{name}\"\nurl = \"https://example.org/{name}.xml\"\n"
            )
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = format!(
            "state_dir = \"/srv/state\"\nspeaker_type = \"local\"\n\
             [spotify]\nclient_id = \"id\"\ndevice = \"Kitchen\"\n\
             [[source]]\ntype = \"music\"\npath = \"/srv/music\"\n\
             [[source]]\ntype = \"podcast\"\nname = \"maus\"\ncache_dir = \"/srv/maus\"\n{}\
             [[source]]\ntype = \"radio\"\n\
             [[source.station]]\nname = \"Kinder\"\nurl = \"https://example.org/k.mp3\"\n\
             [[source]]\ntype = \"podcast\"\nname = \"news\"\ncache_dir = \"/srv/news\"\n{}\
             [[source]]\ntype = \"spotify\"\n\
             [[source.playlist]]\nname = \"A\"\nuri = \"spotify:playlist:aaa\"\n\
             [[source.playlist]]\nname = \"B\"\nuri = \"spotify:playlist:bbb\"\n",
            feed("maus"),
            feed("news")
        );
        std::fs::write(&path, text).unwrap();
        Config::load(&path).unwrap()
    }

    #[test]
    fn podcast_starts_select_podcast_sources_with_shelf_and_cache() {
        let cfg = mixed();
        let got: Vec<_> = podcast_starts(&cfg.sources)
            .iter()
            .map(|s| {
                (
                    s.shelf,
                    s.source.name.as_str(),
                    s.cache,
                    s.settings.feeds[0].name.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (1, "maus", Path::new("/srv/maus"), "maus"),
                (3, "news", Path::new("/srv/news"), "news"),
            ]
        );
    }

    #[test]
    fn only_sources_with_files_are_served_under_their_name() {
        let got = served_roots(&mixed().sources);
        assert_eq!(
            got,
            [
                ("music".to_owned(), PathBuf::from("/srv/music")),
                ("maus".to_owned(), PathBuf::from("/srv/maus")),
                ("news".to_owned(), PathBuf::from("/srv/news")),
            ]
        );
    }

    #[test]
    fn covers_are_fetched_for_spotify_sources_with_playlists() {
        let cfg = mixed();
        let got: Vec<_> = cover_fetches(&cfg.sources)
            .into_iter()
            .map(|f| (f.folder.to_path_buf(), f.uris))
            .collect();
        assert_eq!(
            got,
            [(
                PathBuf::from("/srv/state/spotify-covers"),
                vec![
                    "spotify:playlist:aaa".to_owned(),
                    "spotify:playlist:bbb".to_owned()
                ]
            )]
        );
    }

    #[test]
    fn a_spotify_source_with_no_playlist_is_skipped_between_two_that_fetch() {
        // The config refuses an empty source, so build the sources by hand.
        let spotify = |covers: &str, uris: &[&str]| Source {
            name: "spotify".into(),
            look: crate::config::Look::default(),
            body: SourceBody::Spotify {
                covers: covers.into(),
                playlists: uris
                    .iter()
                    .map(|uri| crate::config::Playlist {
                        name: "p".into(),
                        uri: (*uri).into(),
                        picture: None,
                        color: None,
                    })
                    .collect(),
            },
        };
        let sources = [
            spotify("/srv/a", &["spotify:playlist:a"]),
            spotify("/srv/empty", &[]),
            spotify("/srv/b", &["spotify:playlist:b1", "spotify:playlist:b2"]),
        ];
        let got: Vec<_> = cover_fetches(&sources)
            .into_iter()
            .map(|f| (f.folder.to_path_buf(), f.uris))
            .collect();
        assert_eq!(
            got,
            [
                (
                    PathBuf::from("/srv/a"),
                    vec!["spotify:playlist:a".to_owned()]
                ),
                (
                    PathBuf::from("/srv/b"),
                    vec![
                        "spotify:playlist:b1".to_owned(),
                        "spotify:playlist:b2".to_owned()
                    ]
                ),
            ]
        );
    }

    #[test]
    fn only_podcast_sources_have_feeds_to_check() {
        let cfg = mixed();
        let got: Vec<bool> = cfg
            .sources
            .iter()
            .map(|s| podcast_settings(s).is_some())
            .collect();
        assert_eq!(got, [false, true, false, true, false]);
    }
}
