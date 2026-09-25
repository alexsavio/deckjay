# Internet radio

A `type = "radio"` source is a shelf of stations, one key each. Pressing a
station plays it live on the configured speaker: Chromecast, HEOS or the
local sound output. The code:

- [`src/radio.rs`](../src/radio.rs): `resolve`, from the station's URL to
  its stream.
- [`src/player/cast.rs`](../src/player/cast.rs),
  [`src/player/heos.rs`](../src/player/heos.rs),
  [`src/player/local/`](../src/player/local/mod.rs): one way to play it per
  speaker; [`local/netread.rs`](../src/player/local/netread.rs) reads the
  stream for local playback.
- [`src/config/radio.rs`](../src/config/radio.rs): the `[[source.station]]`
  tables.

## Configuration

```toml
[[source]]
type = "radio"
name = "radio"
picture = "pictures/radio.png"

[[source.station]]
name = "Kinderlieder"
url = "http://streamtdy.ir-media-tec.com/kinderlieder/mp3-128/web/play.mp3"
picture = "pictures/kinderlieder.png"

[[source.station]]
name = "Kids Hits"
url = "https://stream.rpr1.de/kidshits/mp3-128/radiobrowser"
color = "#e8a33d"
```

`url` is the stream itself or a `.pls` / `.m3u` playlist that names it,
over `http://` or `https://`. Each name must be unique in its source.

## From a station to its stream

When the key is pressed, the player thread calls `radio::resolve` with
`net::stream_agent` (10 s to connect, 30 s for the reply headers, no limit
on the body):

1. `GET` the URL. ureq follows redirects. A reply that is not 2xx is an
   error.
2. The `Content-Type` without its parameters decides. A playlist type
   (`audio/x-scpls`, `audio/x-mpegurl`, `application/vnd.apple.mpegurl`,
   ...) is read (at most 64 KiB); `text/plain`, `application/octet-stream`
   or none fall back to the URL's extension (`.pls`, `.m3u`, `.m3u8`). Any
   other type is the stream: its URL and content type are the result, and
   the body is dropped after the headers.
3. An M3U with `#EXT-X-` lines is HLS: the playlist URL is the stream, with
   the content type `application/x-mpegURL`.
4. Otherwise the playlist's first `http(s)` entry (`File1=` first in a
   PLS) is fetched the same way, at most 3 playlists deep.

A failure (no answer, 404, an empty playlist, a loop) fails the key press:
the deck shows the station stopped, and the speaker goes on with what it
played. Error messages and logs leave out the URL's query, which can hold
a listener token.

## On each speaker

| | Chromecast | HEOS | Local |
|---|---|---|---|
| Play | `LOAD` of one live item | `play_stream` of the stream | reads and decodes it |
| Pause | `PAUSE`, else `STOP` | `pause`, else `stop` | silence, stream closed |
| Play again | `LOAD` again | `play_stream` again | connects again |
| Next, Prev | nothing | nothing | nothing |
| Ends when | the media is not ours | a stop that lasts | 3 reconnects fail |

- A station never reports `Progress` or `Finished`: the UI plays it with
  `progress: false`, and every backend starts it that way.
- Play after a pause always joins the station live again, on every
  speaker: a paused HTTP stream stalls, and servers drop stalled listeners.
- **Chromecast**: the item's `contentId` is the resolved stream URL, and
  it is "ours" when the status names that URL. Details:
  [chromecast.md](chromecast.md#radio).
- **HEOS**: the receiver takes 1.5 to 10 s to start a stream and can drop
  to `stop` right after starting, so a stop within 10 s of the start sends
  the stream again, up to 3 `play_stream`s per key press. Details:
  [heos.md](heos.md#radio).
- **Local**: a reader thread with a bounded buffer, a 10 s idle timeout and
  up to 3 new connections in a row. Details:
  [local-audio.md](local-audio.md#radio).

## Formats

| Stream | Chromecast | HEOS | Local |
|---|---|---|---|
| MP3 | yes (Google's list) | yes (spike) | yes, tested |
| AAC-LC | yes (Google's list) | yes (spike) | ADTS only, not tried |
| HE-AAC (`audio/aacp`) | yes (Google's list) | yes (spike) | refused |
| Ogg Vorbis | yes (Google's list) | not tried | should work, not tried |
| HLS | not tried | yes (spike) | refused |
| HTTPS | should work | yes (spike) | yes, tested |

Local playback refuses HLS and `audio/aacp` streams at once, before it opens
the sound card: "this station sends HE-AAC / HLS, which local playback
cannot decode; pick its MP3 stream". Some HE-AAC stations say `audio/aac`;
those fail or sound wrong only while decoding. Opus is not decoded either.

For every speaker, an MP3 stream at 128 kbit/s is the safe choice.

## Finding station URLs

[radio-browser.info](https://www.radio-browser.info/) lists stations with
their stream URL, codec and bit rate. Search by name or tag ("kinder",
"kids", "children"), then prefer entries with codec MP3, a bit rate of 96
to 192 kbit/s and a recent successful check. The station's own web site
often links a `.pls` or `.m3u` ("listen in your player"); both work.

Its API returns JSON, for example:

```sh
curl -s -G 'https://all.api.radio-browser.info/json/stations/search' \
  -d tag=kids -d codec=MP3 -d hidebroken=true -d limit=20 \
  | jq -r '.[] | "\(.name)\t\(.bitrate)\t\(.url_resolved)"'
```

`url_resolved` is the stream after playlists and redirects; `url` is what
the station published. Either can go in `config.toml`.

To check a URL before adding it:

```sh
curl -sI "$URL" | grep -i '^content-type'   # audio/mpeg is best
ffprobe -hide_banner "$URL"                   # codec, rate, channels
```

`just doctor` lists the stations of each radio source; it does not fetch
them.

## Limits

- SHOUTcast v1 servers answer `ICY 200 OK` instead of `HTTP/1.1 200 OK`.
  ureq does not parse that, so resolving fails. Such stations usually have
  an HTTP URL too (`/;stream.mp3`, or the `url_resolved` of
  radio-browser).
- The stream is fetched twice per key press: once to resolve it, once by
  the speaker (or `netread`).
- A slow station server holds the player thread for up to 40 s while it
  resolves (10 s connect, 30 s headers): key presses wait meanwhile.
- A station that cannot be resolved leaves the speaker as it was (Cast and
  HEOS go on with the item before, local playback closes the sound card),
  while the deck shows it stopped.

## Tests and what is unverified

- `radio/tests.rs`: playlists, HLS and errors against a local web server;
  `resolves_real_stations` (ignored) against real stations.
- `heos/tests/radio.rs`: the fake receiver gets `clear_queue` and
  `play_stream` of the resolved URL; the retry after a drop; a lasting
  stop; pause, stop instead of pause, play again; Next and Prev.
- `cast/tests.rs`: which statuses count as our station; the `LOAD`'s media.
- `local/tests/stream.rs`: `netread` against local servers that close, stall
  or answer 404; the fake sound card plays a station, pauses it and joins it
  again; `decodes_real_stations` (ignored) decodes two real MP3 stations.
- `player/tests.rs`: a station that answers 404 reports `Stopped` on every
  speaker.

Not tried on real speakers: everything on Chromecast (pause of a live item,
the `STOP` fallback, HLS, whether the receiver keeps the `contentId`), the
retry, pause and stop on HEOS, and local radio on a real sound card or a
Pi.
