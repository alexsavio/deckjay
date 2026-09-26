# 🎶 kids-deck

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
 [⏮][⏯][⏭][-][+]    - / + = volume, with a level bar, capped by max_volume;
                     in a book or podcast ⏮ ⏭ become ⏪ ⏩: 10 s back or on
```

## ✨ Features

- 🎵 **Many kinds of things to play**, each from its own `[[source]]` in
  `config.toml`: music albums, audiobooks, stories and sound effects from any
  folder or drive, podcasts (it downloads the newest episodes), internet
  radio stations, and Spotify playlists (Premium, played on a Spotify
  Connect device).
- 📚 **Shelves:** each source is a shelf; one key moves to the next shelf, and
  a shelf with more items than keys pages. Decks from the 6-key Mini to the
  32-key XL work; the layout adapts.
- 🎨 **Pictures, no text:** album covers, a `key.png` of your own, or a big
  glyph of the kind (note, book, star, radio waves, microphone) on a colour.
  Small badges tell the kinds apart, a bar shows how far a book got, and a
  dot marks podcast episodes not heard yet.
- 🔖 **Books resume:** audiobooks and podcast episodes play on from where they
  stopped, also after a restart. In them ⏪ and ⏩ jump 10 s back or on
  (`seek_seconds`).
- 🧒 **Made for children:** a volume cap, a power key that stops everything
  and dims the deck (the next press only lights it again), and a deck that
  keeps working when it is unplugged and plugged back in.
- 🧪 **Test without hardware:** a web page that acts as a Stream Deck, and a
  preview picture of the layout.
- 🍓 **Runs on a headless Raspberry Pi** as a service that starts at boot, or
  in Docker.

## 🧰 What you need

- An **Elgato Stream Deck**: MK.2, Scissor Keys, XL, Mini, Neo, Plus or a
  module. The web simulator stands in for it while you try things out.
- A computer next to the speaker: a **Raspberry Pi 3** or newer (64-bit
  Raspberry Pi OS) or a **Mac**.
- A speaker: a **Chromecast built-in** device (tested on a Lenovo smart
  display; built for a JBL Authentics 300), a **Denon or Marantz HEOS**
  receiver (tested on a Denon AVR-X1600H), or the computer's own sound
  output (the Pi's headphone jack, HDMI or a USB sound card).

## 📦 Install

Download the archive for your computer from the
[releases page](https://github.com/alexsavio/kids-music-deck/releases):
Linux x86_64, Linux arm64 (a Raspberry Pi with a 64-bit OS) or macOS on
Apple silicon. Or build it with Rust 1.98 or newer:

```sh
cargo install kids-deck --locked
```

On Linux, install `libasound2-dev`, `libudev-dev` and `pkg-config` before
you build, and install `99-streamdeck.rules` (in the archive and in this
repository; its first lines say how) so the program does not need root.
Then copy `config.example.toml` to `config.toml`, set the speaker and your
music folders, and run `kids-deck` in that folder
(`kids-deck path/to/config.toml` also works). On a Mac, allow Local Network
access as [the development guide](docs/development.md#-run-with-a-stream-deck)
explains. For a Raspberry Pi, follow
[`docs/raspberry-pi.md`](docs/raspberry-pi.md).

## 🔧 How it works

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

## 🍓 Run on a Raspberry Pi

[`docs/raspberry-pi.md`](docs/raspberry-pi.md) installs kids-deck on a
headless Raspberry Pi with Raspberry Pi OS: a service that starts at boot
and starts again when it fails, the music on the SD card, and logs in the
system journal with a size cap. It also shows how to run it in Docker
instead (`just deploy`).

## 💻 Develop

[`docs/development.md`](docs/development.md) sets up the project on your
computer and runs it without a Stream Deck, with the web Stream Deck
simulator (`just sim`), or with a real deck. It also lists the `just`
recipes, the checks before a commit, the release steps and a map of the
code.

## 📝 Notes

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

## 🔌 Speaker protocols

[`docs/heos.md`](docs/heos.md) and [`docs/chromecast.md`](docs/chromecast.md)
describe the HEOS CLI and the Google Cast protocol as kids-deck uses them:
the commands it sends, the order, the quirks of real devices, and how to test
by hand. [`docs/local-audio.md`](docs/local-audio.md) describes local
playback: decoding, sound outputs and the Raspberry Pi's audio devices.

[`docs/podcasts.md`](docs/podcasts.md) explains podcast sources and their
cache, [`docs/radio.md`](docs/radio.md) internet radio stations, and
[`docs/spotify.md`](docs/spotify.md) the Spotify app, sign-in and devices.

## 🤝 Contributing

Issues and pull requests are welcome. Before a pull request, run `just ci`
(format check, clippy with the pedantic lints, tests and API docs) and
keep the Markdown lines within 80 columns (`rumdl check`). Commit messages
follow [Conventional Commits](https://www.conventionalcommits.org), from
which `CHANGELOG.md` is generated. Versions are
[CalVer](https://calver.org) `YYYY.MM.MICRO`; `just release-next` makes a
release. [`docs/development.md`](docs/development.md) has the setup and the
code map; `CLAUDE.md` describes the architecture in more depth.

Code that talks to real devices (Chromecast, HEOS, the USB deck) is tested
against fakes; please say in a pull request what you checked on real
hardware.

## 📄 License

[MIT](LICENSE). The crates kids-deck builds on keep their own licenses;
most are MIT or Apache-2.0, and a few, such as symphonia and
elgato-streamdeck, are MPL-2.0.
