# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with
code in this repository.

## What this is

`kids-deck`, a Rust binary: album covers on an Elgato Stream Deck; pressing
one plays the album on a network speaker, Chromecast built-in (built for a JBL
Authentics 300) or Denon / Marantz HEOS, or on the computer's own sound output
(`speaker_type` = `cast`, `heos` or `local` in `config.toml`).
Develop natively on macOS (Docker Desktop cannot pass USB through), or run
everything in Docker with the web Stream Deck simulator (`just sim`). Docker
also builds the Raspberry Pi 3 (`linux/arm64`) image. `README.md` has setup,
deploy steps and the per-file code map.

## Commands

- `just ci`: fmt check, clippy (`-D warnings`, pedantic), tests, rustdoc
  (`-D warnings`). Must pass before a commit.
- `cargo test <filter>` or `just test-match <filter>`, e.g.
  `cargo test config::` or `cargo test ui::tests::fifteen_keys_with_paging`.
- `just preview [FILE]`: renders a 3x5 (MK.2) layout PNG with the first
  album shown as playing: a still picture, no deck or Docker needed. To
  click through the UI without hardware, use `just sim`.
  Still needs a valid `config.toml` with a `[[source]]`, and a route to
  `speaker_host` (unless `advertise_host` is set).
- `just doctor` (`--check`): sources and their items, connected decks, speaker
  reachability, and with `[spotify]` the account and its Connect devices.
- `just spotify-login` (`kids-deck spotify-login`): the one-time Spotify
  sign-in (OAuth PKCE, `src/spotify/login.rs`); the token goes to
  `<state_dir>/spotify-token.json`, mode 0600. The default log filter keeps
  `ureq_proto` at info, because at trace it logs raw requests with tokens.
- `just sim [MODEL]`: player + simulator in Docker (`compose.sim.yaml`),
  page at <http://localhost:8090>. It passes the Mac's LAN IP as
  `--advertise-host`, because inside a container `local_ip_towards` returns
  the container address, which the speaker cannot reach. Without Docker:
  `cargo run -- simulator` plus `just run --simulator http://localhost:8090`.
- `just debug`: runs with `RUST_LOG=debug,tower_http=debug` (shows each file
  the speaker fetches). In Docker: `RUST_LOG=info,tower_http=debug just sim`.
- macOS: a native `kids-deck` gets "No route to host" to LAN speakers while
  `ping` and `nc` work: that is Local Network privacy blocking the binary
  (Apple binaries are exempt). Grant the terminal Local Network access, or
  test in Docker (`just sim`).
- `just image`, `just deploy`, `just pi-logs`: Pi image build and deploy
  (`PI_HOST` / `PI_DIR` in `.env`). `deploy` never deletes music on the Pi;
  `just pi-music-prune` does (`rsync --delete`, asks first).
- Recipes in the root justfile run in `bash -euo pipefail` (`set shell`), so
  any failing command, also on the left of a pipe, stops the recipe.
- `just test|build|lint|format|typecheck` sit in the claude-files managed
  block and call `.claude/just/rust.just`, a gitignored symlink, so they fail
  without claude-files installed. The project recipes below the block call
  cargo directly. Do not edit the managed block.

Toolchain: `rust-toolchain.toml` pins 1.98, `Cargo.toml` has
`rust-version = "1.98"`, the Dockerfile builds on `rust:1.98-trixie`. Bump
all three together.

## Architecture

Three threads joined by `std::sync::mpsc`; async exists only inside the HTTP
servers:

- **main** (`main.rs` → `ui/`, `deck/`): `DeckSource::open` retries every
  2 s, so the deck can be unplugged. `Ui::run` polls keys every 100 ms, drains
  `PlayerEvent`s, redraws, and returns `Err` when the deck goes away.
- **player** (`player/`, thread `cast`, `heos` or `local`): `main::output` turns
  the config into a `player::Output` (`Cast`/`Heos` carry host and port, `Local`
  only the device name), and `spawn` builds the speaker on the player thread,
  because a sound-card stream cannot move between threads on every platform.
  `mod.rs` owns the command loop for all speaker types: it coalesces commands
  (button mashing; a `SetVolume` before the last `Play` is dropped, since
  `Play` carries the volume), calls the private `Speaker` trait, and polls
  while `poll_interval()` is `Some`. A failed command emits `Stopped` and drops
  the rest of its batch; 3 failed polls in a row end the album the same way.
  `Emitter` drops repeats of the last event, except for the first event after
  each command batch. The network backends share `connect` (every resolved
  address, 3 s timeout); all three share `clamp_volume`.
  - `cast.rs`: connectionless; every command opens a fresh `rust_cast`
    connection (after a TCP connect-timeout probe, because `rust_cast` has no
    timeout) and drops it. Polls every 4 s while an album is active.
  - `heos.rs`: one persistent HEOS CLI connection (TCP 1255, JSON lines;
    the protocol is in `heos/cli.rs`), reconnects after errors.
    `play_stream` plays one URL and HEOS has no queue for URLs, so it polls
    `get_play_state` every second and starts the next track when the state
    goes from `play` to `stop`. A real Denon
    AVR-X1600H needs `clear_queue` before each `play_stream` (else it plays
    a hidden queue of earlier streams and never reports `stop`) and
    `set_play_state stop` when an album ends. These and its other quirks
    are in `docs/heos.md`, each covered by a test in `heos/tests.rs`.
  - `local`: decodes the files itself (`symphonia` 0.6) and plays them with
    `cpal` 0.18 on the output whose name contains `audio_device` (else the
    default). No music server and no speaker address are needed for it.
    Docker Desktop on a Mac has no sound card: test local audio natively.
    Details: `docs/local-audio.md`.
  - Protocol references: `docs/heos.md` and `docs/chromecast.md` (commands,
    sequences, quirks, test-by-hand snippets). Cast was tested on a Lenovo
    smart display (Chromecast built-in); the JBL itself is not tested yet.
- **http** (`server.rs`): single-thread tokio runtime, one axum `ServeDir`
  per source at `/music/<source name>`, behind a middleware that answers 404
  for every path not in `Library::served_files` (the scanned tracks and
  covers): no dotfiles, stray files or symlink escapes reach the LAN. The
  port, the runtime and the listener are set up on the calling thread, so
  start-up failures are errors, not a dead server thread.

Playback flow:

1. `Library::scan` (`library/scan.rs`) builds one `Item` with its `Track`s
   per folder (tracks up to one folder down) or loose audio file of each
   `[[source]]`, and one `Shelf` per source. The deck shows every shelf's
   items one after the other. A source folder that cannot be read is an
   empty shelf and a warning, not an error.
2. `Ui::tracks` turns them into `TrackInfo` URLs with `library::url_for`
   (per-segment percent-encoding) on `base_url`:
   `http://<host>:<http_port>/music`, where `host` is `advertise_host` or
   `main::local_ip_towards(speaker)`.
3. `PlayerCmd::Play` with `Content::Tracks` reaches the speaker backend
   (every backend fails `Content::Stream` and `Content::Spotify` as not
   supported yet, which the loop reports as `Stopped`). Cast launches the
   Default Media Receiver (`CC1AD845`) and loads the whole album as a
   `MediaQueue`. HEOS sets the volume and sends `browse/play_stream` for one
   track; the `url` parameter goes last and unencoded, all other values
   encode `&`, `=`, `%`.
4. The speaker pulls the files itself; this program never streams audio.

State:

- The speaker is the source of truth. Cast treats an album as "ours" only
  when the media `content_id` equals one of our track URLs; anything else
  becomes `Stopped`. Changing URL building changes this match. HEOS has no
  such check: while an album is active it trusts the play state, so a stop
  in the HEOS app looks like the end of a track.
- The UI is optimistic: a key press sets `current` / `playing` at once, and
  later events correct it. Commands, events and `Face`s name items by
  `ItemId`, an index into the `Library` that is never reused while the
  program runs; `ItemKey` (`<source>/<path>`, e.g. `music/01 Animal Songs`)
  is the name that survives a restart.

Deck backends (`deck/`): `Deck` owns the per-`Face` image cache and what each
key shows; the private `Backend` trait does device IO. `hid.rs` is the USB
deck (`convert_image` encoding). `remote.rs` is the simulator client:
blocking `ureq`, PNG encoding, `pressed_keys` is a long-poll
(`GET /api/presses?wait_ms=`). A backend returning `Ok(None)` from open means
"not there yet, keep waiting"; for the simulator that is a transport error
such as "connection refused" or "host not found". An HTTP error status or a
URL ureq cannot use (no `http://`, `https://`) is a real error. A simulator
grid is checked too: at least 2 rows, at most 64 keys, keys of 16 to 512 px.

Simulator (`simulator/`): `kids-deck simulator` runs an axum server on its
own single-thread tokio runtime with the page (`page.html`) and the
`/api/*` routes; the route table is the module doc of `simulator/mod.rs`, and
`Info` / `Brightness` / `State` are shared with the client. The page polls
`/api/state` and reloads a key image when its version changes; versions come
from one counter that never repeats, so a reset cannot hide a change.

Rendering:

- `Face` is both "what a key shows" and the image-cache key. `Deck::show`
  re-sends only keys whose `Face` changed and caches encoded images per
  `Face` until `Deck::retain` drops them (nothing calls it yet), so keep
  the set of distinct faces small (volume is quantised to 20 levels for
  this reason).
- `Layout::new(rows, cols, shelves)` (`ui/layout.rs`): the bottom row holds
  controls (`control_row` picks them by width), the other keys hold the
  items of one shelf. The deck shows only non-empty shelves
  (`Ui::deck_shelves`). One shelf: the last item key becomes "more" when
  the items do not fit. Several shelves: the last item key is the shelf key
  (it shows the next shelf) with "more" just before it; decks with fewer
  than 6 item keys (Mini, Neo, Plus) get one flip key instead, which pages
  and then moves to the next shelf. Decks with fewer than 2 rows are
  ignored.
- `icons/` draws every icon procedurally (4x supersampling); there are no
  image or font assets. `glyphs.rs` has the kind glyphs (note, book, star,
  waves, mic, Spotify); `decorate` adds the "playing" frame, the kind badge,
  the progress bar and the "new" dot to a tile. Item tiles: the item's
  picture (`[source.item]` or `key.png`), else its cover, else the kind
  glyph on its colour. Covers get a kind badge when the deck has shelves of
  more than one kind.

Podcasts (`podcasts/`, `library/podcast.rs`, `ui/podcasts.rs`): each podcast
source has a `podcasts` thread (`podcasts::spawn`) that refreshes its feeds,
downloads into the source's cache folder, adds and removes the files in the
server's live allowlist (`server::Served` via `server::PodcastFiles`), and
sends a `Snapshot` after every refresh. `Library::scan` starts podcast
shelves from `podcasts::load_cached` (no network). The UI takes the newest
snapshot in its loop (`Ui::take_snapshots`), refills the shelf with
`Library::refill` (known `ItemKey`s keep their `ItemId`; items are never
removed from the table), recomputes the shelves on the deck, and drops
cached key images with `Deck::retain`. `Ui::pin_playing` sends the playing
episode's id (`NowPlaying`) so the thread does not delete its file. Episode
keys are `<source>/<feed folder>/<episode id>`; a podcast item without
saved progress shows the "new" dot.

State (`state.rs`): `Store` keeps `<state_dir>/state.json`: the shelf on the
deck, the page of each shelf (by shelf name) and the progress of each item
that resumes (by `ItemKey`). It writes a temp file and renames it, at most
every 10 s (`save_if_due`) and at once on pause and stop. A broken file is
moved to `state.json.bad`; an unwritable folder keeps the state in memory.

Config: `Config` uses `#[serde(deny_unknown_fields)]`. A new key needs a field
and default fn in `config/mod.rs` (or `config/source.rs` for `[[source]]`
keys) and an entry in `config.example.toml`. `deny_unknown_fields` does not
work together with `#[serde(flatten)]`, so source tables list their keys.
`Config::parse` is the testable core; `load` adds file IO and error context.

## Conventions

- Lints live in `Cargo.toml` `[lints]`: `unsafe_code = "forbid"`, clippy
  pedantic (cast lints are allowed for pixel and volume math). Silence a lint
  only with `#[expect(lint, reason = "...")]`.
- Default rustfmt (width 100). A PostToolUse hook runs rustfmt on every
  edited `.rs` file.
- Every module has a `//!` doc. Item docs (`///`) only say what the name and
  code do not: invariants, units, quirks, magic values. `just doc` fails on
  broken intra-doc links.
- Tests are unit tests in `#[cfg(test)]` modules; filesystem tests use
  `tempfile`. `deck/remote.rs` tests run the real simulator server and the
  real `Deck` in one process. `player/heos.rs` is tested against a fake HEOS
  server, not a real receiver. Chromecast's decisions (`poll_event`,
  `skip_target` over `StatusEntry`) are unit-tested in `cast/tests.rs`, but
  its network path and the USB deck (`deck/hid.rs`) have no automated tests
  (Cast was checked by hand on a Lenovo smart display, see
  `docs/chromecast.md`). Say so when a change touches
  them, and verify with `just doctor`, `just sim` or real hardware.
- The UI guesses the result of a key press before the speaker answers, so
  the player must deliver the next event after every command, even when it
  repeats the last one (`player::run` resets `Emitter::last`).
- Keep `Cargo.lock` in git: the Docker build uses `--locked`.
- On macOS, quit the Elgato Stream Deck app before running: it holds the HID
  device.
