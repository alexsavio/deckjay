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
- [`src/player/local/decode.rs`](../src/player/local/decode.rs): file and
  stream decoding (symphonia) and the rate and channel conversion.
- [`src/player/local/netread.rs`](../src/player/local/netread.rs): a radio
  station's stream as a `Read` for symphonia.
- [`src/player/local/output.rs`](../src/player/local/output.rs): the sound
  card (cpal): picking the output, opening the stream, `--check`'s list.
- [`src/player/local/tests.rs`](../src/player/local/tests.rs): decoding of
  WAV, MP3 and AAC files, the converter, and the player with a fake sound
  card; [`tests/resume.rs`](../src/player/local/tests/resume.rs): the start
  track, seeking in every format (the `fixtures/steps.*` files), the
  reported place and the end of a book;
  [`tests/stream.rs`](../src/player/local/tests/stream.rs): `netread`
  against local web servers, and stations on the fake sound card.
- [`src/player/progress.rs`](../src/player/progress.rs): when an item
  reports its place, shared with Cast and HEOS.

Tested on a Mac, with a USB DAC as the default output and the web simulator
as the deck: MP3 and AAC tracks, the end of a track and of the album, Next,
Prev, pause, volume and a wrong `audio_device`. Not tested on a Raspberry Pi
yet. Progress reports, resuming inside a track and radio stations are
tested with the fake sound card only; two real MP3 stations were decoded
through `netread` by an ignored test (`decodes_real_stations`).

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

An album starts at `start.track` (the first track when it is out of
range), at about `start.position` into it: `Source::open_at` seeks with
symphonia's `FormatReader::seek` (`SeekMode::Coarse`, `SeekTo::Time` with
the audio track's id, so an m4b's chapter or cover track does not get in
the way) and resets the decoder. A coarse seek lands on a packet near the
target; the place reported, and counted on from, is where it landed
(`SeekedTo::actual_ts`), not the target. A position past the end of the
track, or a file that cannot seek, plays the track from its beginning, with
a warning.

⏪ and ⏩ (⏮ and ⏭ while an audiobook or a podcast plays) open the current
track again the same way, `seek_seconds` from the place the sound card got
to: 0 at the most, and past the track's end as if it ended (the next track,
or the end of the album). A track that was paused stays paused.

Where seeks land on the 3 s `steps.*` fixtures (`tests/resume.rs`, targets
0.5, 1.5 and 2.5 s):

| Format | Lands | Audio starts |
|---|---|---|
| MP3 | 0.10–0.13 s early | at the reported place, ~0.1 s quiet |
| AAC (`.m4a`, `.m4b`) | up to 0.05 s early | at the reported place |
| FLAC | 0.07–0.12 s early | at the reported place |
| Vorbis | 0.20–0.25 s early | one packet later (0.128 s there) |
| WAV | on a packet boundary before the target | at the reported place |

After a reset the Vorbis decoder turns its first packet into no audio, so
the reported place is that packet early; the MP3 decoder needs a few frames
to fill its bit reservoir. Both are well under a second, which is what a
resumed book needs.

For an item that reports progress (`progress: true`), the place in
the track is what the sound card played: `fill` counts the frames it wrote
for the current generation, so the ~1.5 s the decoder runs ahead does not
count. The track's length comes from the file (the container's duration,
else its frame count and rate). A track start reports at once, the 100 ms
poll at most every 5 s, and a pause at once. When the last track ends by
itself, `Finished` comes before `Stopped`; undecodable tracks at the end of
the album count as its end. Next on the last track and a new album are not
the end.

- **Pause:** `fill` plays silence and takes nothing from the ring, so
  resuming goes on from the same sample.
- **Prev:** restarts the current track once it is 5 s in (a resumed track
  counts from where it resumed), else goes to the previous track (as on
  Chromecast). **Next** on the last track does nothing.
- **Play/pause after the album ended:** starts the album again from the
  first track (an item that reports progress and did not finish: from the
  track it got to).

## Radio

A station (`Content::Stream`) plays like a track that never ends. How the
station's URL becomes the stream URL is in [radio.md](radio.md).

1. **Play:** on the player thread, resolve the station, refuse what
   symphonia cannot decode, connect to the stream (`NetRead::open`: a reply
   that is not 2xx fails at once) and probe it (`Source::open_stream`, with
   the content type as the hint). Only then does the sound card open, so a
   station that fails any of these leaves it closed. Emits `Playing`; no
   `Progress`, no `Finished`.
2. **Reading:** a `radio` thread reads the HTTP body in 16 KiB chunks into
   a bounded channel of 16 chunks (256 KiB, 16 s at 128 kbit/s); the
   decoder thread reads from it. When the body ends or breaks, or nothing
   comes for 10 s, `netread` connects again, at most 3 times in a row
   (`Limits`); a connection that delivered for 30 s resets the count. MP3
   decoding goes on across a new connection (tested); ADTS should too.
   When the reconnects are used up, the stream ends like a track, and the
   next poll ends the station with `Stopped`.
3. **Pause:** the engine stops the stream (silence; the connection
   closes), the sound card stays open. **Play/pause** again connects anew,
   so the station goes on live. **Next / previous** do nothing.
4. **Leaving the station** (another item, a pause, a failure): the engine
   cancels the stream's reads. Without that, a decoder thread waiting for
   a silent station would not see the next item for up to 40 s (10 s idle,
   3 reconnects).

ureq has no timeout between two reads of a body. On a connection that stays
open but sends nothing, the `radio` thread stays blocked in its read until
the server or the network drops the connection; `netread` goes on with a
new thread and connection after 10 s, and the old thread exits at its next
read. The 10 s also cover the new connection itself, so a server that takes
longer than that to answer uses up the reconnects; stations answer in well
under a second.

Formats: MP3 streams (tested, also against two real stations); AAC-LC in
ADTS, Ogg Vorbis and FLAC streams should work but are not tried.
HLS and `audio/aacp` (HE-AAC) are refused at once, before the sound card
opens: "this station sends HE-AAC / HLS, which local playback cannot
decode; pick its MP3 stream". An HE-AAC stream sent as `audio/aac` fails or
sounds wrong only while decoding.

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

`Speaker::stop` (before another speaker takes over) closes the output: the
stream is dropped, which releases the sound card for another program, such
as a Spotify Connect client on the Pi. An item that reports progress first
offers its latest place; the caller reports what follows. The next item
opens the output again.

## Formats

| Files | Codec | Notes |
|---|---|---|
| `.mp3` | MP3 | ID3 tags are skipped |
| `.m4a`, `.m4b`, `.mp4` | AAC-LC | HE-AAC (SBR) is not supported |
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
