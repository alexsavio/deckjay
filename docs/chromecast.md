# Google Cast (Chromecast)

**Tested on one device:** a Lenovo smart display (model CD-4N341Y,
Chromecast built-in), see [Seen on real hardware](#seen-on-real-hardware).
The target speaker, a JBL Authentics 300, is not tested yet. The other facts
below come from the code, the rust_cast 0.21.0 source, a dump of the bytes
rust_cast writes for kids-deck's calls, and Google's docs.

Cast is the default `speaker_type`. The code:

- [`src/player/cast.rs`](../src/player/cast.rs): the rust_cast calls, the
  album queue, radio stations, polling and the "ours" check.
- [`src/player/mod.rs`](../src/player/mod.rs): the command loop shared with
  HEOS, the `Speaker` trait and the `Emitter`;
  [`progress.rs`](../src/player/progress.rs): when an item reports its
  place.
- [`src/config.rs`](../src/config/mod.rs): port 8009 as the default
  `speaker_port` for Cast.
- `print_cast_speaker` in [`src/main.rs`](../src/main.rs): `just doctor`.

There is no fake Cast receiver. `cast/tests.rs` tests the status decisions
(`poll_event`, `skip_target`, `place`, `finished`) on hand-built
`StatusEntry` values and the media a station loads (`Live::to_media`), and
`every_failed_command_reports_stopped` in `mod.rs` sends commands to a
closed port. No test talks Cast V2.

## Protocol (Cast V2)

- **Transport:** TCP port 8009 with TLS. Google lists TCP 8008–8009 for
  casting. kids-deck calls `CastDevice::connect_without_host_verification`:
  rustls with a verifier that accepts any server certificate but still
  checks the handshake signatures. (`CastDevice::connect` would check the
  certificate against the platform roots and the host name.) The device
  authentication namespace (`tp.deviceauth`) is not used.
- **TLS starts late:** rust_cast logs "Connection with host:port
  successfully established." after the TCP connect. The TLS handshake runs
  on the first write, so a TLS failure shows up as an error of the first
  message.
- **Framing:** a 4-byte big-endian length, then a protobuf `CastMessage`
  (openscreen `cast_channel.proto`): `protocol_version` (`CASTV2_1_0`),
  `source_id`, `destination_id`, `namespace`, `payload_type` (`STRING`),
  `payload_utf8` (a JSON text). openscreen's framer rejects bodies over
  64 KiB.
- **Endpoints:** `sender-0` (rust_cast's source id for every message),
  `receiver-0` (the platform receiver), and a running app's `transportId`.
- **Request ids:** rust_cast numbers requests from 1 on each connection and
  waits (`receive_find_map`) for the reply with the same `requestId`. It
  keeps every other message in a buffer. A spontaneous status has
  `requestId` 0 (Google, Media Playback Messages).

Namespaces, all `urn:x-cast:com.google.cast.` plus:

| Namespace | kids-deck sends | Replies it waits for |
|---|---|---|
| `tp.connection` | `CONNECT` | none |
| `tp.heartbeat` | nothing | none (the receiver's `PING`s are buffered) |
| `receiver` | `GET_STATUS`, `SET_VOLUME`, `LAUNCH` | `RECEIVER_STATUS` |
| `media` | `LOAD`, `GET_STATUS`, `PAUSE`, `PLAY`, `SEEK`, `STOP` | `MEDIA_STATUS` |

rust_cast turns `LAUNCH_ERROR` and the media errors `LOAD_FAILED`,
`LOAD_CANCELLED`, `INVALID_PLAYER_STATE` and `INVALID_REQUEST` into a
failed call.

## Messages kids-deck sends

The payloads below are the exact JSON rust_cast 0.21.0 writes, captured by
running its channels against a stream that records the bytes, all on one
connection. Ids 1 to 4 match a kids-deck album start with the Default Media
Receiver not running yet. Ids 5 to 7 and `mediaSessionId: 1` come from the
dump: kids-deck opens a new connection per command, so ids restart at 1,
and it takes `mediaSessionId` from `MEDIA_STATUS`.

| rust_cast call | To | kids-deck reads from the reply |
|---|---|---|
| `connection.connect("receiver-0")` | `receiver-0` | no reply |
| `receiver.get_status()` | `receiver-0` | `applications[]` |
| `receiver.set_volume(level)` | `receiver-0` | nothing |
| `receiver.launch_app(DefaultMediaReceiver)` | `receiver-0` | the app |
| `connection.connect(transportId)` | the app | no reply |
| `media.load_with_queue(…)` | the app | success or failure |
| `media.load_with_opts(…)` (station) | the app | success or failure |
| `media.get_status(transportId, None)` | the app | first entry |
| `media.pause` / `media.play` | the app | success or failure |
| `media.seek(…)` (resume only) | the app | success or failure |
| `media.stop` (station) | the app | success or failure |

```json
{"type":"CONNECT","userAgent":"RustCast"}
{"requestId":1,"type":"GET_STATUS"}
{"requestId":2,"type":"SET_VOLUME","volume":{"level":0.2,"muted":null}}
{"requestId":3,"type":"LAUNCH","appId":"CC1AD845"}
```

From an app entry kids-deck uses `appId`, `sessionId` and `transportId`.
`CC1AD845` is the Default Media Receiver, Google's hosted receiver app
(`DEFAULT_MEDIA_RECEIVER_APPLICATION_ID`); it needs no registration and has
no custom UI. The `LOAD` (wrapped, one queue item shown; `sessionId` and
the destination come from the app entry of `RECEIVER_STATUS`):

```json
{"requestId": 4, "sessionId": "<app sessionId>", "type": "LOAD",
 "media": {"contentId": "http://10.0.0.2:8765/music/Album/01%20Song.mp3",
           "streamType": "BUFFERED", "contentType": "audio/mpeg",
           "metadata": {"metadataType": 3, "title": "01 Song",
                        "albumName": "Album",
                        "images": [{"url": "http://10.0.0.2:8765/…"}]}},
 "currentTime": 0.0, "customData": {}, "autoplay": true,
 "queueData": {"items": [{"autoplay": true,
                          "media": "<same shape as media, one per track>",
                          "playbackDuration": null, "preloadTime": 20.0,
                          "startTime": 0.0}],
               "queueType": "ALBUM", "repeatMode": "REPEAT_OFF",
               "startIndex": 0}}
```

```json
{"requestId":5,"type":"GET_STATUS"}
{"requestId":6,"mediaSessionId":1,"type":"PAUSE","customData":{}}
{"requestId":7,"mediaSessionId":1,"type":"PLAY","customData":{}}
```

- `media` is the start track; `queueData.items` holds every track of the
  album, `startIndex` points at the start track.
- `contentId` is the track URL from `library::url_for`, `contentType` the
  MIME type from the file extension (`library/scan.rs`). `metadataType` 3 is
  `MusicTrackMediaMetadata`. `images` holds the cover URL when the album
  has one.
- rust_cast hard-codes `repeatMode` `REPEAT_OFF`, item `autoplay: true`,
  `preloadTime` 20 s and `startTime` 0.
- `currentTime` is `start.position` in seconds (`LoadOptions.current_time`),
  0.0 unless an item resumes inside a track (see
  [Resume inside a track](#resume-inside-a-track)).
- kids-deck never sends `QUEUE_*`, `CLOSE`, `PING` or `PONG`, `SEEK` only
  right after a `LOAD` that resumes inside a track, and `STOP` only for a
  station the receiver will not pause (see [Radio](#radio)).

`MEDIA_STATUS` fields kids-deck reads (first entry only): `playerState`
(`IDLE`, `PLAYING`, `BUFFERING`, `PAUSED`), `idleReason`, `mediaSessionId`,
`media.contentId`, `media.duration` and `currentTime` (seconds).

## Sequence

Every command opens its own connection and drops it at the end, without a
`CLOSE` (`CastPlayer::open`):

1. **Connect:** a plain TCP connect with a 3 s timeout to each address the
   host resolves to, closed at once, because rust_cast has no connect
   timeout. Then `connect_without_host_verification`, `CONNECT` to
   `receiver-0`, receiver `GET_STATUS`, and look for the app with `appId`
   `CC1AD845`.
2. **Play an album** (`PlayerCmd::Play`): `SET_VOLUME`; `LAUNCH` if the
   Default Media Receiver is not running; `CONNECT` to its `transportId`;
   `LOAD` with the whole album, start index `start.track` (0 when it is out
   of range) and `currentTime` `start.position`; when that is not 0, a
   `SEEK` to it follows. Emits `Playing`; polling starts. The speaker then
   downloads the tracks itself. An item that reports progress first gets
   `Progress` for `start` (the next poll reads the real `currentTime`).
   When the item playing before reports progress, a media `GET_STATUS`
   before the `LOAD` gives its last place.
3. **Poll** every 4 s while an album is active: media `GET_STATUS`. The
   status is "ours" when its `media.contentId` equals one of our track
   URLs. `IDLE` with `loadingItemId` or `extendedStatus` set is the next
   queue item loading (`loading`): nothing is reported and polling goes
   on. `PLAYING` or `BUFFERING` and ours: `Playing`. `PAUSED` and ours:
   `Paused`. Anything else (`IDLE`, no entry, no Default Media Receiver,
   someone else's media): `Stopped`, and polling stops.
   - For an item that reports progress, a status that is ours and
     `PLAYING`, `BUFFERING` or `PAUSED` gives the place (`place`): the
     track by `contentId`, `currentTime`, and `media.duration` as the
     length (missing, 0 or not a number: unknown). The progress policy
     reports at most every 5 s, so with the 4 s poll a playing item
     reports about every 8 s; a new track reports at the first poll that
     sees it.
4. **Next / previous:** media `GET_STATUS`, then find the current track by
   `contentId`. Not ours: nothing. Next on the last track: nothing.
   Otherwise a new `LOAD` of the whole album with the start index moved.
   Previous restarts the current track when `currentTime` is over 5 s
   (`RESTART_THRESHOLD_SECS`), else it goes one track back (the first track
   restarts).
5. **Play/pause key:** media `GET_STATUS`. The next queue item loading:
   nothing is sent, emits `Playing`. `PLAYING` or `BUFFERING`: `PAUSE`,
   emits `Paused` (an item that reports progress first reports the place
   from that status). `PAUSED`: `PLAY`, emits `Playing`. There is no "ours"
   check here: it pauses any media in the Default Media Receiver.
   Otherwise the album starts again with a `LOAD`, from track 1 (an item
   that reports progress: from the track it got to). No album ever started
   and nothing to pause: nothing is sent, emits `Stopped`.
6. **Volume key:** `SET_VOLUME` to `receiver-0`, level clamped to 0.0–1.0.
   This is the device volume, not the stream volume.
7. **End of album:** kids-deck sends nothing. Google documents that a queue
   item with `autoplay` starts "when the item becomes the currentItem", and
   that with `REPEAT_OFF` "When the queue is completed the media session is
   terminated." The next poll should then see `IDLE` (idle reason
   `FINISHED`) or no entry, and report `Stopped`. For an item that reports
   progress, `IDLE` with idle reason `FINISHED` and the `contentId` of our
   last track (`finished`) sends `Finished` before `Stopped`. Without
   `media` in that status, or with no entry (the session terminated), the
   last place a poll saw decides (`near_end`): on the last track, within
   10 s (`END_MARGIN`) of its known length, is `Finished` too. Otherwise
   only `Stopped`, and the item keeps its last place. So a stop from
   another sender in the last 10 s of a book counts as its end, and a book
   whose length the receiver never reported ends only with `FINISHED`.
   This is read from the docs, not seen on a device.
8. **Errors:** a failed command makes `player::run` log a warning, call
   `reset` (polling stops), emit `Stopped` and drop the other key presses
   that came with it: each would open a connection and wait out its own
   timeout. A failed poll is logged at debug level; three in a row
   (`MAX_FAILED_POLLS`) end the album the same way, about 20 s after the
   speaker went away. A poll or command that works restarts the count.

## Radio

A station (`Content::Stream`) loads as one live item, without a queue.
How the station's URL becomes the stream URL is in [radio.md](radio.md).
The `LOAD` (wrapped; from the fields rust_cast 0.21.0 serializes, not a
byte capture):

```json
{"requestId": 4, "sessionId": "<app sessionId>", "type": "LOAD",
 "media": {"contentId": "http://radio.example/kids.mp3",
           "streamType": "LIVE", "contentType": "audio/mpeg",
           "metadata": {"metadataType": 0, "title": "Kids Radio",
                        "images": [{"url": "…"}]}},
 "currentTime": 0.0, "customData": {}, "autoplay": true}
```

- `contentId` is the resolved stream URL; `contentType` is the stream's
  content type, else the station's, else `audio/mpeg`. For HLS it is
  `application/x-mpegURL`. `metadataType` 0 is `GenericMediaMetadata`,
  with the station's name as the title and its cover, if it has one.
- **Play:** resolve the station on the player thread (a failure sends
  nothing and reports `Stopped`), `SET_VOLUME`, `LAUNCH` if needed,
  `CONNECT`, the `LOAD`. Emits `Playing`; polling starts. No `Progress`,
  no `Finished`.
- **Poll:** as for an album, every 4 s. The station is "ours" while
  `media.contentId` equals the stream URL; `IDLE`, no entry or other media
  report `Stopped`.
- **Play/pause key:** `GET_STATUS`. `PLAYING` or `BUFFERING`: `PAUSE`,
  emits `Paused`. When the receiver refuses (an error reply), `STOP`
  instead, and polling stops. `PAUSED`, stopped, or nothing loaded: the
  `LOAD` again, so the station goes on live, not from where it paused.
- **Next / previous:** nothing is sent.

Unverified on a device: all of it. In particular whether the Default Media
Receiver pauses a live item, whether it keeps `contentId` as sent after
following redirects, and whether audio-only HLS plays: packed-audio
segments may need `hlsSegmentFormat`, which rust_cast cannot send.

## Resume inside a track

An item that resumes (an audiobook) sends `PlayerCmd::Play` with
`start = Start { track, position }`. What rust_cast 0.21.0 can send for it
(read in its `channels/media.rs`):

- `load_with_queue` sends `queueData.startIndex` (`MediaQueue.start_index`)
  and the `LOAD`'s `currentTime` (`LoadOptions.current_time`).
- Every queue item goes out with `startTime: 0.0` (`QueueItem::encode`
  hard-codes it) and `queueData` has no `startTime` of its own, so neither
  can carry the position.

Whether a receiver applies the `LOAD`'s `currentTime` to the first queue
item when that item says `startTime` 0 is not documented clearly enough to
rely on. So kids-deck sends both: `currentTime` in the `LOAD`, then a `SEEK`
(`seek_after_load`) to the `mediaSessionId` of the first entry in the
`LOAD`'s reply, with `resumeState` `PLAYBACK_START`. The payload, from the
field order of rust_cast's `PlaybackSeekRequest` (not a byte capture):

```json
{"requestId":5,"mediaSessionId":1,"type":"SEEK",
 "resumeState":"PLAYBACK_START","currentTime":95.5,"customData":{}}
```

- A reply without entries: no `SEEK`, logged at debug level.
- A failed `SEEK` is a warning, not a failed command: the album plays, from
  wherever the `LOAD` put it. The next poll's `currentTime` corrects the
  reported place.
- rust_cast waits for a `MEDIA_STATUS` with the `SEEK`'s `requestId` and the
  same `mediaSessionId`, with no timeout (see "No read timeout" below).
- **Unverified on a device**: whether `currentTime` alone works, and
  whether the `SEEK` right after the `LOAD` lands before the receiver has
  buffered. A double seek to the same spot should at worst cost a short
  rebuffer.

## Seen on real hardware

Lenovo smart display CD-4N341Y, 2026-09-24, through
`just sim` (kids-deck in Docker, the web deck simulator for the keys), with
the two-track test album (a 15 s MP3, then a 15 s m4a with its index at the
start):

1. **Load.** The first album press took 3 s from the key to the loaded
   queue: the device launched the Default Media Receiver (`CC1AD845`).
   With the app already running, a load took about 1 s.
2. **Queue.** The device fetched each file when its track started: the MP3,
   then the m4a 15 s later. kids-deck sent nothing in between; the Cast
   queue moves on by itself.
3. **Keys.** Play/pause paused and resumed. Next went to track 2. Previous,
   pressed 4 s into track 2 (under the 5 s rule), went back to track 1.
4. **Volume.** One volume-up press from `start_volume = 0.2` sent 0.25;
   `just doctor` then showed `volume 25%` and
   `running app: Default Media Receiver (CC1AD845)`.
5. **Track change and end.** The deck kept the album framed through the
   track change, and showed it stopped within one poll (4 s) after the last
   track ended.
6. **Formats.** `audio/mpeg` (MP3) and `audio/mp4` (m4a) both played.

## Quirks and limits

- **No read timeout (accepted limit).** rust_cast sets no timeout
  anywhere, and kids-deck cannot reach its socket:
  `connect_without_host_verification` opens it and keeps it private. The
  3 s probe covers the connect only. A device that accepts the connection
  and then goes silent blocks the player thread until the connection
  closes or fails: no key press gets an answer and no poll runs until
  then. Bounding it would need a worker thread per command that the
  player gives up on; kids-deck does not do that.
- **Idle between tracks.** `IDLE` with `loadingItemId` or `extendedStatus`
  counts as the next track loading, not the end of the album. Taken from
  rust_cast's field docs and Google's `MediaStatus`; not seen on a device.
  There is no time limit on it (HEOS has `LOAD_TIMEOUT`): a receiver that
  stays in that state keeps the deck on "playing".
- **No heartbeat.** kids-deck never sends or answers `PING`. Each
  connection lives for one command, so it relies on the command finishing
  before the receiver gives up on it.
- **The "ours" check is a string compare.** `contentId` must come back
  exactly as sent. Changing `advertise_host`, `http_port` or
  `library::url_for` makes an album loaded earlier look foreign
  (`Stopped`). Whether a receiver ever rewrites `contentId` is unverified.
- **`media` may be missing.** Google marks `MediaStatus.media` optional,
  and rust_cast's docs say it comes only when the media changed. A
  `GET_STATUS` reply without it would read as "not ours" and report
  `Stopped`. Unverified on a device.
- **Strict parsing.** rust_cast knows four player states and needs
  `mediaSessionId`, `playbackRate`, `playerState` and
  `supportedMediaCommands` in every status entry. Another state or a
  missing field fails the call, and kids-deck reports `Stopped`.
- **Message size.** One `LOAD` carries every track. Measured with
  rust_cast 0.21.0 and 46-character URLs: 100 tracks make a 33 605-byte
  frame, 200 tracks 66 905 bytes, over the 64 KiB limit in openscreen's
  framer. Longer URLs lower the limit. Derived from the framer source, not
  tested on a device.
- **Another sender takes over.** The JBL One app, Spotify Connect or the
  Google Home app replace the app or its media; the next poll reports
  `Stopped`.
- **Lag.** Changes made elsewhere reach the deck up to 4 s later.
- **Formats.** Google lists FLAC (up to 96 kHz / 24-bit), HE-AAC, LC-AAC,
  MP3, Opus, Vorbis, WAV (LPCM) and WebM for Chromecast Audio, Google Home
  and Google Home Mini. The JBL's list is unverified.
- **`sessionId` in `LOAD`.** rust_cast sends it; Google's `LoadRequestData`
  reference does not list it. Whether a receiver needs it is unverified.
- **`SET_VOLUME` source.** Google's Media Playback Messages page covers
  the `media` namespace only. The receiver-namespace `SET_VOLUME` shown here
  comes from rust_cast and matches Chromium's `cast_message_util.cc`, which
  sends it on the receiver namespace to `receiver-0`.

## Test by hand

`just doctor` (`kids-deck --check`) runs step 1 of the sequence and prints
the receiver status (format from `print_cast_speaker`):

```text
Speaker 192.168.1.50:8009 (Cast):
  reachable ✓  volume 20%
  running app: <displayName> (<appId>)
```

It never launches an app or loads media, so a clean result proves the
network path, TLS and the receiver namespace, not playback. A failure
prints `NOT reachable: <error>`. Against a local port that accepts TCP but
does not speak TLS it printed `NOT reachable: unexpected end of file`.

To watch the messages:

- `just debug` sets `RUST_LOG=debug,tower_http=debug`, which includes
  rust_cast's `Message sent: …` and `Message received: …` lines (each
  `CastMessage` in Rust debug form) and every file request from the
  speaker.
- rust_cast gives rustls a `KeyLogFile`, so
  `SSLKEYLOGFILE=/tmp/keys.log just doctor` writes the TLS keys to that
  file and Wireshark can decrypt port 8009 with them.

On macOS a self-built binary can get "No route to host" from Local
Network privacy: see item 8 of
[Seen on real hardware](heos.md#seen-on-real-hardware) in the HEOS doc.

First checks on a real device, in order:

1. `just doctor`: `reachable ✓` and the volume.
2. Press an album. Expect `playing album=… track=…` in the log, then a
   `GET` of track 1 in the HTTP trace (`started processing request`).
3. Let a track end: a `GET` of the next track, and the deck stays on
   "playing". With `preloadTime` 20 the `GET` may come up to 20 s early.
4. Pause, play, next, previous, volume.
5. Let the album end: the deck shows it stopped within 4 s.

## References

Google:

- [Media Playback Messages][messages]: the `media` namespace, `LOAD`,
  `PLAY`, `PAUSE`, `GET_STATUS`, `MEDIA_STATUS`, player states, idle
  reasons, error messages, `requestId` 0.
- [Queueing][queueing], [`QueueData`][queuedata], [`QueueItem`][queueitem]
  (`autoplay`, `preloadTime`), [`cast.framework.messages`][fwmessages]
  (`REPEAT_OFF`, `PlayerState`, `IdleReason`),
  [`MediaStatus`][mediastatus], [`LoadRequestData`][loadrequest].
- [Web Receiver Overview][webreceiver] (Default Media Receiver),
  [`CastMediaControlIntent`][intent] (`CC1AD845`).
- [Supported Media][media], [Network requirements for cast
  moderator][network] (TCP 8008–8009).

Protocol sources:

- openscreen [`cast_channel.proto`][proto] (message fields, `sender-0`,
  `receiver-0`), [`message_framer.cc`][framer] (length prefix, 64 KiB),
  [`message_util.h`][msgutil] (namespaces).
- Chromium [`cast_message_util.cc`][chromium] (`SET_VOLUME` to
  `receiver-0`).
- [rust_cast 0.21.0][rustcast]: `lib.rs`, `message_manager.rs`,
  `channels/{connection,heartbeat,receiver,media}.rs`, `cast/proxies.rs`.

[messages]: https://developers.google.com/cast/docs/media/messages
[queueing]: https://developers.google.com/cast/docs/web_receiver/queueing
[queuedata]: https://developers.google.com/cast/docs/reference/web_receiver/cast.framework.messages.QueueData
[queueitem]: https://developers.google.com/cast/docs/reference/web_receiver/cast.framework.messages.QueueItem
[fwmessages]: https://developers.google.com/cast/docs/reference/web_receiver/cast.framework.messages
[mediastatus]: https://developers.google.com/cast/docs/reference/web_receiver/cast.framework.messages.MediaStatus
[loadrequest]: https://developers.google.com/cast/docs/reference/web_receiver/cast.framework.messages.LoadRequestData
[webreceiver]: https://developers.google.com/cast/docs/web_receiver
[intent]: https://developers.google.com/android/reference/com/google/android/gms/cast/CastMediaControlIntent
[media]: https://developers.google.com/cast/docs/media
[network]: https://support.google.com/chrome/a/answer/12256492
[proto]: https://chromium.googlesource.com/openscreen/+/refs/heads/main/cast/common/channel/proto/cast_channel.proto
[framer]: https://chromium.googlesource.com/openscreen/+/refs/heads/main/cast/common/channel/message_framer.cc
[msgutil]: https://chromium.googlesource.com/openscreen/+/refs/heads/main/cast/common/channel/message_util.h
[chromium]: https://chromium.googlesource.com/chromium/src/+/refs/heads/main/components/media_router/common/providers/cast/channel/cast_message_util.cc
[rustcast]: https://docs.rs/rust_cast/0.21.0/rust_cast/
