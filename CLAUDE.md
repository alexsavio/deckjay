# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with
code in this repository.

## What this is

`kids-deck`, a Rust binary: album covers on an Elgato Stream Deck; pressing
one plays the album on a Chromecast speaker (built for a JBL Authentics 300).
Develop natively on macOS (Docker Desktop cannot pass USB through); Docker
only builds the Raspberry Pi 3 (`linux/arm64`) image. `README.md` has setup,
deploy steps and the per-file code map.

## Commands

- `just ci`: fmt check, clippy (`-D warnings`, pedantic), tests, rustdoc
  (`-D warnings`). Must pass before a commit.
- `cargo test <filter>` or `just test-match <filter>`, e.g.
  `cargo test config::` or `cargo test ui::tests::fifteen_keys_with_paging`.
- `just preview [FILE]`: renders a 3x5 (MK.2) layout PNG with the first
  album shown as playing. The only way to see UI changes without hardware.
  Still needs a valid `config.toml`, an existing `music_dir`, and a route to
  `speaker_host` (unless `advertise_host` is set).
- `just doctor` (`--check`): albums, connected decks, speaker reachability.
- `just debug`: runs with `RUST_LOG=debug,tower_http=debug` (shows each file
  the speaker fetches).
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
server:

- **main** (`main.rs` → `ui.rs`, `deck.rs`): retries `Deck::open` every 2 s,
  so the deck can be unplugged. `Ui::run` polls keys every 100 ms, drains
  `PlayerEvent`s, redraws, and returns `Err` when the deck goes away.
- **cast** (`player.rs`): consumes `PlayerCmd`, emits deduplicated
  `PlayerEvent`s. Connectionless: every command opens a fresh `rust_cast`
  connection (after a TCP connect-timeout probe, because `rust_cast` has no
  timeout) and drops it. Polls speaker status every 4 s only while an album
  is active. `coalesce` drops commands made stale by later ones (button
  mashing).
- **http** (`server.rs`): single-thread tokio runtime, axum `ServeDir` under
  `/music`. The port is bound on the main thread so "port in use" fails at
  startup.

Playback flow:

1. `library::scan` builds `Album` / `Track` from the music folder.
2. `Ui::tracks` turns them into `TrackInfo` URLs with `library::url_for`
   (per-segment percent-encoding) on `base_url`:
   `http://<host>:<http_port>/music`, where `host` is `advertise_host` or
   `main::local_ip_towards(speaker)`.
3. `PlayerCmd::PlayAlbum` reaches `CastPlayer::load`, which launches the
   Default Media Receiver (`CC1AD845`) and loads the whole album as a
   `MediaQueue`.
4. The speaker pulls the files itself; this program never streams audio.

State:

- The speaker is the source of truth. The player treats an album as "ours"
  only when the media `content_id` equals one of our track URLs; anything
  else becomes `Stopped`. Changing URL building changes this match.
- The UI is optimistic: a key press sets `current` / `playing` at once, and
  later events correct it. Album ids in commands and events are indexes into
  `Ui::albums`.

Rendering:

- `Face` is both "what a key shows" and the image-cache key. `Deck::show`
  re-sends only keys whose `Face` changed and caches encoded images per
  `Face` forever, so keep the set of distinct faces small (volume is
  quantised to 20 levels for this reason).
- `Layout::new(rows, cols, albums)`: the bottom row holds controls
  (`control_row` picks them by width), the other keys hold albums; with too
  many albums the last album key becomes "more" and pages. Decks with fewer
  than 2 rows are ignored.
- `icons.rs` draws every icon procedurally (4x supersampling); there are no
  image or font assets.

Config: `Config` uses `#[serde(deny_unknown_fields)]`. A new key needs a field
and default fn in `config.rs` and an entry in `config.example.toml`.
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
  `tempfile`. Code that talks to the deck or the speaker has no automated
  tests (only `coalesce` is covered): say so when a change touches `deck.rs`
  or `player.rs`, and verify with `just doctor` or real hardware.
- Keep `Cargo.lock` in git: the Docker build uses `--locked`.
- On macOS, quit the Elgato Stream Deck app before running: it holds the HID
  device.
