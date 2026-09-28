# 🎶 deckjay

[![CI](https://github.com/alexsavio/deckjay/actions/workflows/ci.yml/badge.svg)](https://github.com/alexsavio/deckjay/actions/workflows/ci.yml)
[![zizmor](https://github.com/alexsavio/deckjay/actions/workflows/zizmor.yml/badge.svg)](https://github.com/alexsavio/deckjay/actions/workflows/zizmor.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A music player with no screen: press a picture on an Elgato Stream Deck,
and that album, audiobook, story, podcast, radio station or Spotify
playlist plays. Made for small children who cannot read yet, and just as
good for anyone who wants their music one press away.

It plays on a Chromecast built-in speaker, a Denon or Marantz HEOS
receiver, a Spotify Connect device, or the computer's own sound output.

It runs on Linux (a Raspberry Pi next to the speaker, or any PC) and on
macOS. Written in Rust.

![The web Stream Deck simulator: eight album keys, the first one playing, a
power key, the next-shelf key, and the playback and volume keys
below](docs/images/simulator.webp)

```text
 [A][A][A][A][⏻]     A = an album, book or story: press to play, again to pause
 [A][A][A][A][S]     S = the next shelf; ⏻ = power: stops all, dims the deck
 [⏮][⏯][⏭][-][+]    - / + = volume, with a level bar, capped by max_volume;
                     in a book, podcast or story ⏮ ⏭ show ⏪ ⏩: 10 s back or on
```

## ✨ Features

- 🎵 **Many kinds of things to play**, each from its own `[[source]]` in
  `config.toml`: music albums, audiobooks, stories and sound effects from any
  folder or drive, podcasts (it downloads the newest episodes), internet
  radio stations, and Spotify playlists (Premium, played on a Spotify
  Connect device).
- 📚 **Shelves:** each source is a shelf; one key moves to the next shelf, and
  a shelf with more items than keys pages. Decks from the 6-key Mini to the
  36-key Plus XL work; the layout adapts.
- 🎨 **Pictures, no text:** album covers, a `key.png` of your own, or a big
  glyph of the kind (note, book, star, radio waves, microphone) on a colour.
  Small badges tell the kinds apart, a bar shows how far a book got, and a
  dot marks podcast episodes not heard yet.
- 🔖 **Books resume:** audiobooks and podcast episodes play on from where they
  stopped, also after a restart. In books, podcasts and stories ⏪ and ⏩
  jump 10 s back or on (`seek_seconds`).
- 🧒 **Simple on purpose:** a volume cap, a power key that stops everything
  and dims the deck (the next press only lights it again), a deck that
  keeps working when it is unplugged and plugged back in, and goes dark
  when the program stops. No menus: small children and grandparents can
  use it.
- 🧪 **Test without hardware:** a web page that acts as a Stream Deck, and a
  preview picture of the layout.
- 🍓 **Runs on a headless Raspberry Pi** as a service that starts at boot, or
  in Docker.

## 🧰 What you need

- An **Elgato Stream Deck** with screens and at least two rows of keys:
  the original, MK.2, Scissor Keys, Mini, Neo, XL, Plus, Plus XL, or an
  MK.2, Mini or XL module (not the Pedal: it has no screens). Tested on a
  real MK.2; the others through their key layouts and the web simulator,
  which also stands in for a deck while you try things out.
- **Other decks** (for example from Ajazz, Mirabox or Loupedeck) are not
  supported yet, and pull requests that add one are welcome: the device
  code sits behind a small `Backend` trait in `src/deck/` (`hid.rs` for
  USB decks, `remote.rs` for the simulator), so a new deck is one new
  backend.
- A computer for the Stream Deck, on the same network as the speaker:
  **Linux** (a **Raspberry Pi 3** or newer or a Pi Zero 2 W, with 64-bit
  Raspberry Pi OS, or a PC) or a **Mac**. Windows is not supported.
- A speaker: a **Chromecast built-in** device (tested on a Lenovo smart
  display; built for a JBL Authentics 300), a **Denon or Marantz HEOS**
  receiver (tested on a Denon AVR-X1600H), or the computer's own sound
  output (the Pi's headphone jack, HDMI, a USB sound card, or a paired
  Bluetooth speaker: see
  [`docs/raspberry-pi.md`](docs/raspberry-pi.md#-play-on-a-bluetooth-speaker),
  not tested yet).

## 📦 Install

Get the program one of two ways:

- **Download** the archive for your computer from the
  [releases page](https://github.com/alexsavio/deckjay/releases): Linux
  x86_64, Linux arm64 (a Raspberry Pi with a 64-bit OS) or macOS on Apple
  silicon. The Linux ones need glibc 2.35 or newer (Debian 12, Ubuntu
  22.04, Raspberry Pi OS 12 or later), `libasound2` and `libudev1`.
- **Build** it with Rust 1.98 or newer. On Linux, install
  `libasound2-dev`, `libudev-dev` and `pkg-config` first.

  ```sh
  cargo install deckjay --locked
  ```

Then:

1. Copy `config.example.toml` (in the archive and in this repository) to
   `config.toml`, and set the speaker and your music folders.
2. On Linux, install `99-streamdeck.rules` (its first lines say how), so
   the program can use the deck without root.
3. Run `deckjay` in that folder (`deckjay path/to/config.toml` also
   works). On a Mac, quit the Elgato Stream Deck app first, and allow
   Local Network access as
   [the development guide](docs/development.md#-run-with-a-stream-deck)
   explains.

For a Raspberry Pi without a screen, follow
[`docs/raspberry-pi.md`](docs/raspberry-pi.md): it installs deckjay as a
service that starts at boot.

## 🔧 How it works

The program serves your music folders over HTTP and tells the speaker which
file to play. The speaker downloads the files itself, so playback keeps going
even if the computer is busy.

- **Chromecast:** gets the whole album as a queue.
- **HEOS:** plays one file at a time, so the program starts the next track
  when one ends (with a gap of about a second).
- **Local audio** (`speaker_type = "local"`): the program decodes and plays
  the files itself, on the sound output that `audio_device` names.

`config.example.toml` explains every setting. The main ones:

- `speaker_type`: `"cast"`, `"heos"` or `"local"`.
- `speaker_host`: the speaker's IP address.
- `max_volume`: the cap for the volume keys.
- One `[[source]]` table per folder, podcast, radio or Spotify shelf.

## 🍓 Run on a Raspberry Pi

[`docs/raspberry-pi.md`](docs/raspberry-pi.md) installs deckjay on a
headless Raspberry Pi with Raspberry Pi OS: a service that starts at boot
and starts again when it fails, the music on the SD card, and logs in the
system journal with a size cap.

Most steps also fit another Linux computer with systemd. The guide also
shows how to run it in Docker instead (`just deploy`).

## 💻 Develop

[`docs/development.md`](docs/development.md) sets up the project on your
computer and runs it without a Stream Deck, with the web Stream Deck
simulator (`just sim`), or with a real deck.

It also lists the `just` recipes, the checks before a commit, the release
steps and a map of the code.

## 📝 Notes

- **Tested on:** macOS on Apple silicon with a Stream Deck MK.2; Linux in
  CI (Ubuntu) and in Docker with the web simulator, playing on a Lenovo
  smart display (Cast). The Raspberry Pi guide is not tested on a real Pi
  yet.
- **Volume cap** only applies to the deck's keys. The speaker's own buttons,
  its app (JBL One, HEOS) and voice assistants can still go louder.
- **HEOS** (tested on a Denon AVR-X1600H):
  - Turn on "Network Control: Always On" on the receiver, or it cannot be
    reached in standby.
  - `just doctor` lists the HEOS players the device knows. The program uses
    the player whose IP is `speaker_host`, else the first one.
  - It cannot tell a track that ended from a track stopped in the HEOS app:
    both start the next track.
- **"No route to host" on macOS** while `ping` works: macOS blocks the
  program's local network access. Allow Local Network access for your
  terminal app, or run with `just sim` in Docker.
- **Speaker IP:** give the speaker a fixed address (DHCP reservation in your
  router), otherwise the config breaks when the IP changes.
- **Formats:** mp3, m4a/aac, flac, ogg/opus, wav. The speaker must be able
  to play the format: check your model's list for ogg/opus and flac. Local
  playback cannot decode Opus; it skips those tracks.

## 📖 More docs

- [`docs/heos.md`](docs/heos.md) and
  [`docs/chromecast.md`](docs/chromecast.md): the HEOS CLI and the Google
  Cast protocol as deckjay uses them: the commands it sends, the order,
  the quirks of real devices, and how to test by hand.
- [`docs/local-audio.md`](docs/local-audio.md): local playback: decoding,
  sound outputs and the Raspberry Pi's audio devices.
- [`docs/podcasts.md`](docs/podcasts.md): podcast sources and their cache.
- [`docs/radio.md`](docs/radio.md): internet radio stations.
- [`docs/spotify.md`](docs/spotify.md): the Spotify app, sign-in and
  devices.

## 🤝 Contributing

Issues and pull requests are welcome. Before a pull request:

- Run `just ci`: format check, clippy with the pedantic lints, tests and
  API docs.
- Keep the Markdown lines within 80 columns (`rumdl check`).
- Write the commit messages as
  [Conventional Commits](https://www.conventionalcommits.org):
  `CHANGELOG.md` is generated from them.
- Code that talks to real devices (Chromecast, HEOS, the USB deck) is
  tested against fakes: say in the pull request what you checked on real
  hardware.

Versions are [CalVer](https://calver.org) `YYYY.MM.MICRO`;
`just release-next` makes a release.
[`docs/development.md`](docs/development.md) has the setup and the code
map, and `CLAUDE.md` describes the architecture in more depth.

## 📄 License

[MIT](LICENSE). The crates deckjay builds on keep their own licenses;
most are MIT or Apache-2.0, and a few, such as symphonia and
elgato-streamdeck, are MPL-2.0.
