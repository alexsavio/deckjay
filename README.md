# kids-deck

A music player for kids: album covers on an Elgato Stream Deck, music on a
Chromecast-enabled speaker (built for a JBL Authentics 300). Written in Rust.

```text
 [A][A][A][A][A]     A = album cover, press to play (press again to pause)
 [A][A][A][A][>]     > = more albums (only if they don't fit on one page)
 [⏮][⏯][⏭][-][+]    - / + = volume, with a level bar, capped by max_volume
```

The program serves your music folder over HTTP and tells the speaker to play
an album as a queue. The speaker streams the files itself, so playback keeps
going even if the computer is busy.

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
4. Put music in `music/<album>/` with a `cover.jpg` per album.
5. Check everything:

   ```sh
   just doctor
   ```

   It lists your albums, the Stream Decks it sees, and whether the speaker
   answers. `just preview` draws the key layout into `layout.png`.
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
| `just doctor` | List albums, Stream Decks and speaker status |
| `just preview [FILE]` | Draw the key layout into a PNG, no hardware needed |
| `just ci` | Format check, clippy, tests and docs: run before a commit |
| `just test-match NAME` | Run only the tests whose name contains `NAME` |
| `just doc --open` | Build and open the API docs |
| `just deploy` | Build the Pi image and start it on the Pi |

The code targets Rust 1.98 (edition 2024). `cargo clippy` uses the
pedantic lint group; the lint settings are in `Cargo.toml`.

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
  the JBL One app and voice assistants can still go louder.
- **Speaker IP:** give the speaker a fixed address (DHCP reservation in your
  router), otherwise the config breaks when the IP changes.
- **Formats:** mp3, m4a/aac, flac, ogg/opus, wav — what the Chromecast
  default receiver plays.

## Code map

| File | What it does |
|---|---|
| `main.rs` | Startup, `--check` / `--preview`, reconnecting to the deck |
| `config.rs` | `config.toml` loading and validation |
| `library.rs` | Scans album folders, finds covers, builds URLs |
| `ui.rs` | Key layout, what each key shows and does |
| `icons.rs` | Draws control icons and album tiles (no image files needed) |
| `deck.rs` | Stream Deck access, image caching, key presses |
| `player.rs` | Chromecast control on its own thread |
| `server.rs` | HTTP server the speaker downloads the music from |
