# Local audio out

With `speaker_type = "local"` in `config.toml`, kids-deck plays the album
itself, on the sound output of the computer it runs on: the headphone jack or
HDMI of a Raspberry Pi, or the speakers of a Mac. There is no network
speaker, no `speaker_host` and no music web server. The code:

- [`src/player/local/mod.rs`](../src/player/local/mod.rs): `LocalPlayer`,
  the album walk behind the same `Speaker` trait as Cast and HEOS.
- [`src/player/local/engine.rs`](../src/player/local/engine.rs): the
  decoder thread, the ring buffer and `Sink::fill`, which the sound card
  calls. No sound card needed, so the tests drive it.
- [`src/player/local/decode.rs`](../src/player/local/decode.rs): file
  decoding (symphonia) and the rate and channel conversion.
- [`src/player/local/output.rs`](../src/player/local/output.rs): the sound
  card (cpal): picking the output, opening the stream, `--check`'s list.
- [`src/player/local/tests.rs`](../src/player/local/tests.rs): decoding of
  WAV, MP3 and AAC files, the converter, and the player with a fake sound
  card.

Tested on a Mac, with a USB DAC as the default output and the web simulator
as the deck: MP3 and AAC tracks, the end of a track and of the album, Next,
Prev, pause, volume and a wrong `audio_device`. Not tested on a Raspberry Pi
yet.

## Configuration

```toml
speaker_type = "local"
# Part of the output's name, as `just doctor` lists it; left out, the
# system's default output plays.
audio_device = "Headphones"
```

`audio_device` picks the first output whose name contains it, ignoring
case, in the order `just doctor` (`--check`) prints them: the default output
first. A value that matches nothing fails the album press, and the log
lists every output name. The volume keys scale the samples: 1.0 plays the
file as it is, 0.5 halves the amplitude.

## How it plays

Three parts run at the same time:

- **Player thread** (the shared command loop in `player/mod.rs`): opens the
  file of the next track and hands it to the decoder thread. It opens the
  file itself, so a track that cannot be decoded is skipped at once, with a
  warning in the log. While an album is active it polls every 100 ms for
  the end of the track, so tracks follow each other with a gap of at most
  about 0.1 s.
- **Decoder thread** (`local-decoder`): decodes the current track, converts
  it to the output's format and fills a lock-free ring (rtrb) with about
  1.5 s of audio, in chunks of whole frames.
- **Sound card callback** (cpal's audio thread): `Sink::fill` copies the
  ring into the card's buffer and applies the volume. It takes no lock,
  allocates nothing and never waits; with nothing in the ring it plays
  silence.

Each new track (album start, Next, Prev, the next track) gets a new
generation number, and every chunk carries the number of its track. `fill`
drops chunks of an older generation the moment the number changes, so Next
cuts to the new track at once instead of playing out the buffer. After a
track's last chunk comes an end marker; when `fill` reaches it, the track
has been heard to the end and the next poll starts the next track, or ends
the album with `Stopped`.

- **Pause:** `fill` plays silence and takes nothing from the ring, so
  resuming goes on from the same sample.
- **Prev:** restarts the current track once it has played 5 s, else goes to
  the previous track (as on Chromecast). **Next** on the last track does
  nothing.
- **Play/pause after the album ended:** starts the album again from the
  first track.

## The output

The sound card opens at the first album press, not at start up: a computer
without one runs as usual and fails only when an album is pressed (the
deck shows the album stopped). Then the stream stays open, playing silence
between albums.

The stream uses the output's default format (rate, channels, sample type);
kids-deck never asks the output to change it. Decoded audio is fitted to
it:

- **Rate:** linear interpolation when the file's rate differs, for example
  a 44.1 kHz file on a 48 kHz output. Cheap enough for a Pi 3.
- **Channels:** mono plays on the first two channels, stereo (or the front
  pair of a surround file) on the first two; any other output channels
  stay silent. A one-channel output gets the mix of left and right.

If the stream breaks (a USB output unplugged, the sound server gone), the
next command or poll fails and the deck shows the album stopped. The next
album press opens the output again. Dropouts (xruns) and the default output
changing on a Mac do not stop playback: they are only logged at debug
level.

## Formats

| Files | Codec | Notes |
|---|---|---|
| `.mp3` | MP3 | ID3 tags are skipped |
| `.m4a`, `.mp4` | AAC-LC | HE-AAC (SBR) is not supported |
| `.aac` | AAC-LC in ADTS | |
| `.flac` | FLAC | |
| `.ogg`, `.oga` | Vorbis | |
| `.wav` | PCM | |

**Opus is not supported**: symphonia has no Opus decoder. The library scan
still lists `.opus` files, so on local audio each one is skipped with a
warning, and the album goes on with the next track. The same goes for ALAC
in `.m4a` and for broken files. Convert Opus albums for local playback, for
example `ffmpeg -i in.opus -c:a libmp3lame -q:a 2 out.mp3`.

## Raspberry Pi 3

The Pi 3 has two outputs:

- **Headphones**: the 3.5 mm jack, the card named `bcm2835 Headphones`.
- **HDMI**: named `vc4-hdmi` with the KMS video driver (Raspberry Pi OS
  default), `bcm2835 HDMI 1` with the older firmware driver.

On Linux one card appears as several ALSA devices with the same name
(`hw:`, `plughw:`, `sysdefault:`, `dmix:`, `hdmi:` ...), so `--check` shows
the ALSA device in brackets, for example
`bcm2835 Headphones, bcm2835 Headphones (plughw:CARD=Headphones,DEV=0)`.
`audio_device` matches the whole line, so it can name one ALSA device:

- `audio_device = "Headphones"`: the first device of the jack.
- `audio_device = "plughw:CARD=Headphones"`: the jack through ALSA's
  converting plugin.
- `audio_device = "hdmi:CARD=vc4hdmi"`: HDMI. The `vc4-hdmi` hardware takes
  only IEC958 frames, which its `hw:` and `plughw:` devices cannot make;
  its `hdmi:` and `sysdefault:` devices can.

Without `audio_device` the output is ALSA's `default` device, card 0 unless
`/etc/asound.conf` says otherwise. Run `just doctor` on the Pi to see the
names.

## Docker

- **Pi:** `docker-compose.yml` runs the container privileged with the
  host's `/dev` mounted, so `/dev/snd` is there. The image has the ALSA
  library (`libasound2t64`); the build stage needs `libasound2-dev`.
  kids-deck talks to ALSA directly, not through a sound server, so run it
  on Raspberry Pi OS Lite, or stop the host's PipeWire or PulseAudio: a
  sound server holds the card, and the album press fails with the device
  busy.
- **Mac:** Docker Desktop has no sound card, so `just sim` with
  `speaker_type = "local"` only shows albums failing. Test natively
  instead, with the simulator in Docker or not:

```sh
cargo run -- simulator                          # one terminal
cargo run -- config.toml --simulator http://localhost:8090
```

Check the log: `sound output open` names the output and its format, one
`playing` line follows each track, `end of the album` follows the last one,
and `skipping <file>: ...` names a track that could not be decoded.
The default log filter (`info,symphonia=error`) hides symphonia's own lines,
such as the harmless `skipped 4 bytes of junk at 0` warning that comes with
every `.m4a` file; `RUST_LOG=info` shows them again.
