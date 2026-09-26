# 💻 Development

How to build, run and test kids-deck on your computer. You need neither a
Stream Deck nor a speaker to start: the web simulator stands in for the
deck, and `speaker_type = "local"` plays on the computer's own sound
output.

## 🚀 Set up

1. Install [rustup](https://rustup.rs) and [just](https://just.systems) 1.43
   or newer (`brew install rustup just` on a Mac). The first `cargo`
   command installs the Rust version pinned in `rust-toolchain.toml`. On
   Linux, also install `libasound2-dev`, `libudev-dev` and `pkg-config`.
2. Run `just setup`. It creates `config.toml` from `config.example.toml`,
   and the `music` and `state` folders.
3. Put music in `music/<album>/` with a `cover.jpg` per album. For
   audiobooks or stories, or music in other folders or on other drives,
   add a `[[source]]` table per folder at the end of `config.toml` (see
   `config.example.toml`).
4. Pick the speaker: `speaker_type = "local"` needs nothing more; for a
   Chromecast or a HEOS receiver, set `speaker_type` and `speaker_host`.
5. Check everything:

   ```sh
   just doctor
   ```

   It lists your sources and what is in them, the Stream Decks it sees,
   and the sound outputs (local audio) or whether the speaker answers.

## 🧪 Test without a Stream Deck

`kids-deck simulator` is a web page that acts as a Stream Deck: it shows
the key images, and a click on a key is a key press. The player uses it in
place of the USB deck when you start it with `--simulator URL`.

- **All in Docker:** `just sim`, then open <http://localhost:8090>. The
  player and the simulator run in two containers. Pick another deck with
  `just sim xl` (models: `mk2` 3×5, `mini` 2×3, `neo` 2×4, `xl` 4×8,
  `plus` 2×4). Stop with Ctrl-C and `just sim-down`. Sources outside
  `./music` need a volume at the same path: put it in
  `compose.sim.local.yaml` (git ignores it; `just sim` adds it; the
  example is in `compose.sim.yaml`).
- **Without Docker:** `cargo run -- simulator` in one terminal and
  `just run --simulator http://localhost:8090` in another. Use this for
  local audio: Docker Desktop on a Mac has no sound card.
- **A still picture:** `just preview` draws the key layout into
  `layout.png`, with the first item shown as playing.

A network speaker downloads the music from this computer, so `just sim`
passes the computer's LAN address to the player (set `HOST_IP` to
override it) and publishes port 8765.

## 🎹 Run with a Stream Deck

Docker Desktop cannot hand USB devices to containers, so on a Mac run the
program natively:

1. **Quit the Elgato Stream Deck app** (menu bar icon → Quit). It holds on
   to the device.
2. Run it:

   ```sh
   just run
   ```

macOS asks two things the first time; allow both:

- **Incoming network connections** for `kids-deck`: the speaker downloads
  the music from your Mac.
- **Local Network access** for your terminal app (System Settings →
  Privacy & Security → Local Network): needed to talk to the speaker.
  Without it the program gets "No route to host" while `ping` works.

On Linux, install `99-streamdeck.rules` (its first lines say how), so the
program can use the deck without root.

Ctrl-C (or closing the terminal) stops the program and turns the deck
dark; `kids-deck --blank` does the same for a deck left lit.

`just debug` logs more, including each file request the speaker makes.

## 📋 Commands

Run `just` to list every recipe. The ones you need most:

| Command | What it does |
|---|---|
| `just run` | Start the player |
| `just doctor` | List sources and their items, Stream Decks and speaker status |
| `just sim [MODEL]` | Run the player with the web Stream Deck simulator |
| `just preview [FILE]` | Draw the key layout into a PNG, no hardware needed |
| `just ci` | Format check, clippy, tests and docs: run before a commit |
| `just test-match NAME` | Run only the tests whose name contains `NAME` |
| `just doc --open` | Build and open the API docs |
| `just install` | Install `kids-deck` into `~/.cargo/bin` |
| `just spotify-login` | Sign in to Spotify once |
| `just deploy` | Build the Pi image and start it on the Pi in Docker |

The code targets Rust 1.98 (edition 2024). `cargo clippy` uses the
pedantic lint group; the lint settings are in `Cargo.toml`.

## ✅ Before a commit

- `just ci`: format check, clippy, the tests and the API docs.
- `rumdl check README.md CLAUDE.md docs/*.md`: Markdown lines stay within
  80 columns.
- Commit messages follow
  [Conventional Commits](https://www.conventionalcommits.org);
  `CHANGELOG.md` is generated from them.
- Code that talks to real devices (Chromecast, HEOS, the USB deck) is
  tested against fakes. Check a change to it with `just doctor`,
  `just sim` or the real device.

## 🔖 Releases

Versions are [CalVer](https://calver.org) `YYYY.MM.MICRO`, for example
`2026.9.0`, tagged `v2026.9.0`. `just release-next` (or
`just release VERSION`) sets the version, runs `just ci`, writes
`CHANGELOG.md`, commits, tags and pushes. The tag starts the Release
workflow: it builds the binaries for Linux (x86_64, arm64) and macOS
(Apple silicon), makes the GitHub release, and publishes the crate to
crates.io. `just publish-dry` checks the package first.

## 🧭 Code map

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
| `net.rs` | HTTPS agents for feeds, radio and Spotify; the address the speaker downloads from (`BaseUrl`) |

`CLAUDE.md` describes the architecture in more depth: the threads, the
playback flow, the state and the rendering. The other files in `docs/`
describe the speaker protocols ([heos.md](heos.md),
[chromecast.md](chromecast.md)), [local audio](local-audio.md),
[podcasts](podcasts.md), [radio](radio.md) and [Spotify](spotify.md).
