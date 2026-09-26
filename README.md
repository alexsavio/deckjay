# kids-deck

[![CI](https://github.com/alexsavio/kids-music-deck/actions/workflows/ci.yml/badge.svg)](https://github.com/alexsavio/kids-music-deck/actions/workflows/ci.yml)
[![zizmor](https://github.com/alexsavio/kids-music-deck/actions/workflows/zizmor.yml/badge.svg)](https://github.com/alexsavio/kids-music-deck/actions/workflows/zizmor.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A music player for small children who cannot read yet: they press a
picture on an Elgato Stream Deck, and the music plays. The music plays on a
Chromecast built-in speaker, a Denon or Marantz HEOS receiver, or the
computer's own sound output. It runs on a Raspberry Pi next to the speaker,
or on a Mac. Written in Rust.

```text
 [A][A][A][A][⏻]     A = an album, book or story: press to play, again to pause
 [A][A][A][A][S]     S = the next shelf; ⏻ = power: stops all, dims the deck
 [⏮][⏯][⏭][-][+]    - / + = volume, with a level bar, capped by max_volume
```

## Features

- **Many kinds of things to play**, each from its own `[[source]]` in
  `config.toml`: music albums, audiobooks, stories and sound effects from any
  folder or drive, podcasts (it downloads the newest episodes), internet
  radio stations, and Spotify playlists (Premium, played on a Spotify
  Connect device).
- **Shelves:** each source is a shelf; one key moves to the next shelf, and
  a shelf with more items than keys pages. Decks from the 6-key Mini to the
  32-key XL work; the layout adapts.
- **Pictures, no text:** album covers, a `key.png` of your own, or a big
  glyph of the kind (note, book, star, radio waves, microphone) on a colour.
  Small badges tell the kinds apart, a bar shows how far a book got, and a
  dot marks podcast episodes not heard yet.
- **Books resume:** audiobooks and podcast episodes play on from where they
  stopped, also after a restart.
- **Made for children:** a volume cap, a power key that stops everything
  and dims the deck (the next press only lights it again), and a deck that
  keeps working when it is unplugged and plugged back in.
- **Test without hardware:** a web page that acts as a Stream Deck, and a
  preview picture of the layout.
- **Runs on a Raspberry Pi** in Docker, with the image built on your
  computer.

## What you need

- An **Elgato Stream Deck**: MK.2, Scissor Keys, XL, Mini, Neo, Plus or a
  module. The web simulator stands in for it while you try things out.
- A computer next to the speaker: a **Raspberry Pi 3** or newer (64-bit
  Raspberry Pi OS) or a **Mac**.
- A speaker: a **Chromecast built-in** device (tested on a Lenovo smart
  display; built for a JBL Authentics 300), a **Denon or Marantz HEOS**
  receiver (tested on a Denon AVR-X1600H), or the computer's own sound
  output (the Pi's headphone jack, HDMI or a USB sound card).

## How it works

The program serves your music folders over HTTP and tells the speaker which
file to play. The speaker downloads the files itself, so playback keeps going
even if the computer is busy. A Chromecast gets the whole album as a queue. A
HEOS device plays one file at a time, so the program starts the next track
when one ends (with a gap of about a second). With `speaker_type = "local"`
the program decodes and plays the files itself, on the sound output that
`audio_device` names.

`config.example.toml` explains every setting. The main ones:
`speaker_type` (`"cast"`, `"heos"` or `"local"`), `speaker_host` (the
speaker's IP address), `max_volume`, and one `[[source]]` table per folder,
podcast, radio or Spotify shelf.

## Develop on macOS

Docker Desktop can't hand USB devices to containers, so on a Mac run the
program natively. Docker is only used to build the Raspberry Pi image.

1. Install [rustup](https://rustup.rs) and [just](https://just.systems) 1.43
   or newer (`brew install rustup just`). The first `cargo` command installs
   the Rust version pinned in `rust-toolchain.toml`.
2. **Quit the Elgato Stream Deck app** (menu bar icon → Quit). It holds on
   to the device.
3. Run `just setup`. It creates `config.toml` from `config.example.toml`.
   Set `speaker_host` to the speaker's IP.
4. Put music in `music/<album>/` with a `cover.jpg` per album. For
   audiobooks or stories, or music in other folders or on other drives, add
   a `[[source]]` table per folder at the end of `config.toml` (see
   `config.example.toml`).
5. Check everything:

   ```sh
   just doctor
   ```

   It lists your sources and what is in them, the Stream Decks it sees, and
   whether the speaker answers. `just preview` draws the key layout into `layout.png`.
6. Run it:

   ```sh
   just run
   ```

macOS will ask two things the first time; allow both:

- **Incoming network connections** for `kids-deck`: the speaker downloads the
  music from your Mac.
- **Local Network access** for your terminal app (System Settings → Privacy &
  Security → Local Network): needed to talk to the speaker.

Debug logging: `just debug`. You'll see each file request the speaker makes.

## Commands

Run `just` to list every recipe. The ones you need most:

| Command | What it does |
|---|---|
| `just run` | Start the player |
| `just doctor` | List sources and their items, Stream Decks and speaker status |
| `just preview [FILE]` | Draw the key layout into a PNG, no hardware needed |
| `just ci` | Format check, clippy, tests and docs: run before a commit |
| `just test-match NAME` | Run only the tests whose name contains `NAME` |
| `just doc --open` | Build and open the API docs |
| `just sim [MODEL]` | Run the player with a web Stream Deck simulator |
| `just deploy` | Build the Pi image and start it on the Pi |
| `just spotify-login` | Sign in to Spotify once (`pi-spotify-login` on the Pi) |

The code targets Rust 1.98 (edition 2024). `cargo clippy` uses the
pedantic lint group; the lint settings are in `Cargo.toml`.

## Test without a Stream Deck

`kids-deck simulator` is a web page that acts as a Stream Deck: it shows the
key images, and a click on a key is a key press. The player uses it in place
of the USB deck when you start it with `--simulator URL`.

- **All in Docker:** `just sim`, then open <http://localhost:8090>. The
  player and the simulator run in two containers. Pick another deck with
  `just sim xl` (models: `mk2` 3×5, `mini` 2×3, `neo` 2×4, `xl` 4×8,
  `plus` 2×4). Stop with Ctrl-C and `just sim-down`. Sources outside
  `./music` need a volume at the same path: put it in
  `compose.sim.local.yaml` (git ignores it; `just sim` adds it; the
  example is in `compose.sim.yaml`).
- **Without Docker:** `cargo run -- simulator` in one terminal and
  `just run --simulator http://localhost:8090` in another.

Playback still needs the real speaker. It downloads the music from this
computer, so `just sim` passes the computer's LAN address to the player
(set `HOST_IP` to override it) and publishes port 8765.

## Deploy to the Raspberry Pi 3

Install **64-bit** Raspberry Pi OS Lite and Docker on the Pi. Then on the Mac:

```sh
just deploy
just pi-logs
```

`just deploy` builds the arm64 image, copies it to the Pi with
`docker save | ssh docker load`, copies `docker-compose.yml`, `config.toml`
and `music/` to `~/kids-deck`, and runs `docker compose up -d` there. It uses
`pi@raspberrypi.local`; set `PI_HOST` (and `PI_DIR`) in `.env` to change it.

Sources outside `music/` are not copied: put them on the Pi (or a drive
mounted there) at the path in `config.toml`, and add a volume for each in
`docker-compose.yml`.

`just deploy` only adds and updates music on the Pi; it never deletes. To
remove albums from the Pi that you deleted on the Mac, run
`just pi-music-prune` (it asks first).

The container restarts automatically after reboots and keeps looking for the
Stream Deck, so it can be unplugged and plugged back in.

Without Docker, copy the binary and run it as a systemd service instead, and
install `99-streamdeck.rules` so it doesn't need root.

## Notes

- **Supported decks:** anything the `elgato-streamdeck` crate knows with at
  least two rows: MK.2, Scissor Keys, XL, Mini, Neo, Plus and the modules.
  The layout adapts to the number of keys.
- **Volume cap** only applies to the deck's keys. The speaker's own buttons,
  its app (JBL One, HEOS) and voice assistants can still go louder.
- **HEOS:** `just doctor` lists the HEOS players the device knows. The
  program uses the player whose IP is `speaker_host`, else the first one. It
  cannot tell a track that ended from a track stopped in the HEOS app: both
  start the next track. Tested on a Denon AVR-X1600H. Turn on "Network
  Control: Always On" on the receiver, or it cannot be reached in standby.
- **"No route to host" on macOS** while `ping` works: macOS blocks the
  program's local network access. Allow Local Network access for your
  terminal app, or run with `just sim` in Docker.
- **Speaker IP:** give the speaker a fixed address (DHCP reservation in your
  router), otherwise the config breaks when the IP changes.
- **Formats:** mp3, m4a/aac, flac, ogg/opus, wav. The speaker must be able
  to play the format: check your model's list for ogg/opus and flac. Local
  playback cannot decode Opus; it skips those tracks.

## Speaker protocols

[`docs/heos.md`](docs/heos.md) and [`docs/chromecast.md`](docs/chromecast.md)
describe the HEOS CLI and the Google Cast protocol as kids-deck uses them:
the commands it sends, the order, the quirks of real devices, and how to test
by hand. [`docs/local-audio.md`](docs/local-audio.md) describes local
playback: decoding, sound outputs and the Raspberry Pi's audio devices.

[`docs/podcasts.md`](docs/podcasts.md) explains podcast sources and their
cache, [`docs/radio.md`](docs/radio.md) internet radio stations, and
[`docs/spotify.md`](docs/spotify.md) the Spotify app, sign-in and devices.

## Code map

| File | What it does |
|---|---|
| `main.rs` | Startup, command-line options, reconnecting to the deck |
| `config/` | `config.toml` loading and validation (`mod.rs`); `[[source]]` tables (`source.rs`) |
| `library/` | Items and shelves, one shelf per source (`mod.rs`); scans source folders and finds covers (`scan.rs`); builds URLs |
| `ui/` | What each key shows and does (`mod.rs`), key layout with shelves (`layout.rs`) |
| `icons/` | Draws control icons, kind glyphs and key decorations (no image files needed) |
| `state.rs` | What the deck remembers in `state.json`: shelf, pages, audiobook progress |
| `deck/` | Image caching and key presses, for a USB deck (`hid.rs`) or the simulator (`remote.rs`) |
| `simulator/` | The web Stream Deck simulator (`kids-deck simulator`) |
| `player/` | The player thread: shared command loop (`mod.rs`), when to report progress (`progress.rs`), speaker or Spotify (`router.rs`), Chromecast (`cast.rs`), HEOS (`heos.rs`, protocol in `heos/cli.rs`), local sound output (`local`, radio streams in `local/netread.rs`), Spotify Connect (`spotify.rs`) |
| `server.rs` | HTTP server the speaker downloads the music from; its allowlist changes as podcasts refresh |
| `podcasts/` | The podcasts thread: feeds, download plan, cache and refresh |
| `spotify/` | Spotify sign-in, saved login, Web API client and playlist covers |
| `radio.rs` | Turns a station URL (or its `.pls` / `.m3u`) into the stream |
| `net.rs` | HTTPS agents for feeds, radio and Spotify |

## Contributing

Issues and pull requests are welcome. Before a pull request, run `just ci`
(format check, clippy with the pedantic lints, tests and API docs) and
keep the Markdown lines within 80 columns (`rumdl check`). Commit messages
follow [Conventional Commits](https://www.conventionalcommits.org), from
which `CHANGELOG.md` is generated. `CLAUDE.md` describes the architecture in
more depth.

Code that talks to real devices (Chromecast, HEOS, the USB deck) is tested
against fakes; please say in a pull request what you checked on real
hardware.

## License

[MIT](LICENSE). The crates kids-deck builds on keep their own licenses;
most are MIT or Apache-2.0, and a few, such as symphonia and
elgato-streamdeck, are MPL-2.0.
