# HEOS CLI

kids-deck drives Denon / Marantz HEOS devices through the HEOS CLI, set by
`speaker_type = "heos"` in `config.toml`. The code:

- [`src/player/heos.rs`](../src/player/heos.rs): the player choice, the
  album walk and radio stations.
- [`src/player/heos/cli.rs`](../src/player/heos/cli.rs): the protocol.
- [`src/player/heos/tests.rs`](../src/player/heos/tests.rs): a fake receiver
  on 127.0.0.1 that records every command line kids-deck sends, and can
  send progress events for
  [`tests/progress.rs`](../src/player/heos/tests/progress.rs);
  [`tests/radio.rs`](../src/player/heos/tests/radio.rs) plays stations on
  it.
- [`src/player/mod.rs`](../src/player/mod.rs): the command loop shared with
  Cast, the `Speaker` trait and the `Emitter`;
  [`progress.rs`](../src/player/progress.rs): when an item reports its
  place.
- [`src/config.rs`](../src/config/mod.rs): `speaker_type`, and port 1255 as the
  default `speaker_port` for HEOS.

Tested on a Denon AVR-X1600H.

## Protocol

- **Transport:** plain TCP to port 1255 of any HEOS device ("a telnet
  connection", spec §2). No TLS, no login for the commands kids-deck uses.
  One device controls every player of the HEOS system.
- **Commands:** one ASCII line, `heos://<group>/<command>?<key>=<value>&…`,
  ended by `\r\n` (spec §3.1).
- **Replies and events:** one JSON object per line, ended by `\r\n`
  (spec §3.2).
- **Encoding:** `&`, `=` and `%` inside a value become `%26`, `%3D` and
  `%25`, in commands and in replies. One exception: the `url` pair of
  `browse/play_stream` "should be the last attribute value pair … to handle
  url_path with special characters and command delimiters" (spec §4.4.10).
  kids-deck sends it last and unencoded, as pyheos does (`command_line`).
  Track URLs are already percent-encoded per path segment by
  `library::url_for`.

A reply, as the AVR-X1600H sends it (wrapped; on the wire it is one
line):

```json
{"heos": {"command": "player/get_play_state", "result": "success",
          "message": "pid=-1234567890&state=pause"}}
```

- `message` holds `key=value` pairs joined by `&`. The command's arguments
  come back in it.
- Some replies also have a `payload` (a JSON list or object), for example
  `player/get_players`.
- A failure has `"result": "fail"` and
  `"message": "eid=<code>&text=<text>&<arguments>"` (spec §6.1).
- A slow command first gets a `success` reply whose message is
  `command under process`, then the real reply (spec §3.2).
  `Reply::answers` skips the first one.
- The spec's examples pad command names (`" player/ set_volume "`) and quote
  values (`pid='1'`). The AVR-X1600H does neither. kids-deck accepts both
  (`unspaced`, `unquote`).
- Change events (`event/…` lines) are off on a new connection (spec
  §4.1.1). kids-deck still sends `enable=off`, as the start-up sequence in
  spec §2.1.1 advises, and polls instead. The one exception is the place
  in a track: no command returns it, only the change event
  `event/player_now_playing_progress` (spec §5), with `pid`, `cur_pos` and
  `duration` in milliseconds. So while an item that reports progress plays
  (`progress: true`), events are on for that connection. `Cli::exchange`
  keeps the latest progress event of our player and skips every other line
  that does not answer the command it sent.

Error codes that matter here (spec §6.2):

| eid | Text | Where |
|---|---|---|
| 4 | Requested data not available | `clear_queue` on an empty queue |
| 13 | Processing previous command | not seen on hardware |
| 14 | cannot play | not seen on hardware; the fake receiver uses it |

## Commands kids-deck sends

`HeosPlayer::call` puts `pid=<pid>` first on every player command.

| Command | Sent | kids-deck reads |
|---|---|---|
| `system/register_for_change_events?enable=off` | new connection | result |
| `system/register_for_change_events?enable=on` | progress item | result |
| `player/get_players` | new connection, `just doctor` | payload |
| `player/set_volume?pid=P&level=L` | item start, volume key | result |
| `player/clear_queue?pid=P` | before each `play_stream` | nothing |
| `browse/play_stream?pid=P&url=U` | each track or station | result |
| `player/get_play_state?pid=P` | poll, play/pause key | `state` |
| `player/set_play_state?pid=P&state=S` | play/pause, item end, stop | result |

- `level` is `round(volume × 100)`, volume clamped to 0.0–1.0 (spec range
  0 to 100).
- `S` is `pause` or `play` for the play/pause key, `stop` at the end of an
  album or station. A station is paused with `pause`, and with `stop` when
  the receiver answers `fail` to that; it never gets `play` (see
  [Radio](#radio)).
- A `fail` reply to `clear_queue` (eid 4 on an empty queue) is logged at
  debug level and ignored. An I/O error (timeout, reset) ends the track
  change: no `play_stream` goes out without its `clear_queue`.
- `enable=on` goes before the first command after a progress item's track
  started (usually the first poll), and again after a reconnect. `enable=off`
  goes before the next command once that item is no longer active: the
  `set_play_state stop` at its end, or the next album. Albums that do not
  report progress never turn events on, so their command lines are the ones
  below.
- kids-deck never sends `get_queue`, `get_now_playing_media`,
  `play_next`, `play_previous` or `heart_beat`.

`Connection::open` picks the `get_players` entry whose `ip` is
`speaker_host` or the socket's peer address, else the first entry. `pid`
can be a JSON number or a string, and it can be negative. The AVR-X1600H
answers (wrapped, serial left out):

```json
{"heos": {"command": "player/get_players", "result": "success",
          "message": ""},
 "payload": [{"name": "Den", "pid": -1234567890,
              "model": "Denon AVR-X1600H", "version": "3.139.173",
              "ip": "192.168.1.40", "network": "wired", "lineout": 0,
              "serial": "…"}]}
```

## Sequence

The lines of the test `play_album_sets_the_volume_then_streams_the_first_track`
(pid 7, URL shortened):

```text
heos://system/register_for_change_events?enable=off
heos://player/get_players
heos://player/set_volume?pid=7&level=20
heos://player/clear_queue?pid=7
heos://browse/play_stream?pid=7&url=http://10.0.0.2:8765/music/A/01.m4a
```

1. **Connect** (on the first command, and on the next one after an I/O
   error): TCP connect with a 3 s timeout to each address the host
   resolves to, 5 s read and write timeouts,
   `register_for_change_events?enable=off`, `get_players`, pick the pid.
   The connection stays open. When a command on that open connection
   finds the socket closed or reset (not a timeout), kids-deck connects
   again and sends the command once more: the CLI resets idle connections
   when it recovers from a hang (item 6), and a restarted receiver has
   none. Every command it sends is safe to send twice.
2. **Play an album** (`PlayerCmd::Play`): `set_volume`, then the start
   track (`start.track`; track 1 when it is out of range): `clear_queue`,
   `play_stream`. Emits `Playing`. The track is "loading". An item that
   reports progress first gets `Progress` for the start of that track.
   `start.position` is not used: the CLI has no command that seeks in a
   stream, so the track (an audiobook chapter) starts from its beginning,
   and kids-deck logs that at debug level.
3. **Poll** every 1 s while an album is active: `get_play_state`.
   - For an item that reports progress, the latest
     `player_now_playing_progress` read since the last poll gives the place
     (`Progress`, at most every 5 s). `duration=0` means the length is
     unknown.
   - `play`: the track is "started"; emits `Playing`. `pause`: "started";
     emits `Paused`.
   - `stop` or `unknown` while loading, for less than 15 s: wait. After
     15 s: log "the track did not start playing" and end the album.
   - `stop` or `unknown` after "started": `clear_queue` and `play_stream`
     for the next track, or end the album after the last one.
   - Any other state is an error.
4. **Next / previous:** `clear_queue` and `play_stream` for the track after
   or before the current one. Next on the last track does nothing. Previous
   on the first track restarts it. There is no "restart the track after
   5 s" rule (Cast has one). Without an active album both do nothing.
5. **Play/pause key:** `get_play_state` first. `play` → `set_play_state`
   `pause`; `pause` → `set_play_state` `play`. `stop` while the track loads
   (under 15 s) sends nothing more and reports `Playing`. In every other case
   the album starts again from track 1 (an item that reports progress: from
   the track it got to): without an active album, what the receiver plays
   is not known to be ours. No album ever started: emits `Stopped`.
6. **Volume key:** `set_volume`.
7. **End of album:** `set_play_state?state=stop`, emits `Stopped`, polling
   stops. For an item that reports progress, `Finished` comes before
   `Stopped` when the last track stopped after it started, unless the last
   progress event put it more than 10 s (`END_MARGIN`) before the track's
   length: that was a stop pressed in the HEOS app, and the item keeps its
   place. With the length unknown, every such stop counts as the end.
8. **Errors:** a failed command (I/O error, timeout, `result: fail`) makes
   `player::run` log a warning, call `reset` (the album is no longer
   active), emit `Stopped` and drop the other key presses that came with
   it: each would wait out its own timeout. A `fail` reply keeps the
   connection; an I/O error drops it and the next call reconnects. A
   failed poll is logged at debug level; three in a row
   (`MAX_FAILED_POLLS`) end the album the same way, about 12 to 18 s after
   the speaker went away. A poll or command that works restarts the
   count. A failed `play_stream` from a poll ends the album.
9. **Stop** (`Speaker::stop`, before another speaker takes over): only
   while an album or station is active. `set_play_state stop` (an item that
   reports progress first offers its latest place); polling stops, and the
   caller reports what follows. A failure is logged as a warning and
   changes nothing else.

## Radio

A station (`Content::Stream`) is one live stream, played with the same
`clear_queue` and `play_stream` as a track. How the station's URL becomes
the stream URL is in [radio.md](radio.md).

A spike on the AVR-X1600H found that HEOS plays MP3, AAC and HE-AAC
streams, over HTTP, HTTPS and HLS, and follows 302 redirects. A stream
takes 1.5 to 10 s to start, with the state going `stop`, `unknown`, then
`play`, and it can drop to `stop` right after it started.

1. **Play:** resolve the station on the player thread (a failure sends
   nothing to the receiver and reports `Stopped`), `set_volume`,
   `clear_queue`, `play_stream` with the stream URL. Emits `Playing`. The
   station never reports `Progress` or `Finished`.
2. **Poll** every 1 s, as for a track. `play` and `pause` report
   `Playing` and `Paused`. `stop` or `unknown`:
   - before the stream was seen playing, for less than 15 s
     (`LOAD_TIMEOUT`): wait; after that: end it.
   - within 10 s (`DROP_WINDOW`) of the stream first seen playing: a drop.
     `clear_queue` and `play_stream` again, while the deck stays on
     "playing", up to 3 `play_stream`s per key press (`MAX_TRIES`).
   - later, after the play/pause key paused it, or with the tries used up:
     a stop that lasts. End it.
   - Ending sends `set_play_state stop` (item 4 of "Seen on real hardware")
     and emits `Stopped`. There is no next track.
3. **Play/pause key:** `get_play_state` first. `play`: `set_play_state
   pause`; when the receiver answers `fail`, `set_play_state stop` instead,
   and polling stops. Either way emits `Paused`. `pause`, or nothing of
   ours playing: `clear_queue` and `play_stream` again, so the station goes
   on live, not from where it paused.
4. **Next / previous:** nothing is sent.
5. **Volume key:** `set_volume`.

The lines for a station and a press of play/pause while it plays (pid 7):

```text
heos://player/set_volume?pid=7&level=20
heos://player/clear_queue?pid=7
heos://browse/play_stream?pid=7&url=http://radio.example/kids.mp3
heos://player/get_play_state?pid=7
heos://player/set_play_state?pid=7&state=pause
```

Not tried on the receiver: the retry after a drop, pausing a live stream,
and the `stop` fallback. A stop pressed in the HEOS app within 10 s of the
start looks like a drop, and the station starts again.

## Power key

HEOS has no power command. The deck's power key first stops playback as a
switch to another speaker does (`set_play_state stop`), then connects to
the receiver's own control port, 23, and sends `PWSTANDBY` followed by a
carriage return: the Denon and Marantz text protocol for "standby". That
port answers only with **Network Control** set to **Always On** (Setup,
Network). A HEOS speaker without that port refuses the connection; the
power key then only stops it, and the log has a warning.

Not tested on the AVR-X1600H yet. By hand:

```sh
printf 'PWSTANDBY\r' | nc -w 2 192.168.1.40 23
```

## Limits of the design

Written down in the module doc of `heos.rs`:

- `play_stream` plays one URL and there is no queue for URLs, so kids-deck
  walks the album itself. It polls `get_play_state` every second instead of
  listening to change events: one connection, and the only event it parses
  is the progress event.
- There is a gap of about 1 s between tracks: the end shows up at the next
  poll, then the next stream loads.
- A stop pressed in the HEOS app looks like the end of a track: the next
  track starts. The receiver cannot tell "our" stream from another one
  (item 5 of "Seen on real hardware"). On the last track of an item that
  reports progress, a stop in its last 10 s, or with its length unknown,
  counts as the end (`Finished`).
- The place in a track is as old as the latest progress event, which comes
  about every 5 s (item 5). Progress events were seen on the AVR-X1600H;
  kids-deck's use of them is tested against the fake receiver only.
- No seek: an item that resumes starts its track from the beginning. A
  one-file book (`.m4b`) therefore starts over; a book split into chapter
  files goes back to the start of the chapter. For the same reason ⏮ and ⏭
  keep skipping tracks (chapters) on HEOS, also in an audiobook, a podcast
  or a story, where the other speakers jump `seek_seconds`.
- A track that does not start within 15 s (`LOAD_TIMEOUT`) ends the album.
- One connection stays open. The spec (§2.1.1) says the CLI module sleeps
  until the first connection comes, and advises controllers that reconnect
  to keep an idle connection open. Up to 32 connections (spec §2.1.3).
- Timeouts: 3 s to connect to each address; 5 s for each read, each
  write, and the whole wait for one reply. A key press gets its answer
  within one of these timeouts, plus a poll already under way (up to 5 s).
- During a CLI hang (item 6) the album ends after three failed polls,
  while the receiver plays the current track to its end.

## Seen on real hardware

Denon AVR-X1600H, pid -1234567890, firmware `3.139.173`, 2026-09-24. Where
the code handles an item, the test in `heos/tests.rs` is named.

1. **Hidden stream queue.** `browse/play_stream` puts the URL in a queue.
   During the playback tests `player/get_queue` listed 0 items. After our
   track ended, the receiver played earlier stream URLs by itself (the HTTP
   log showed fetches of URLs from earlier tests) and kept reporting
   `state=play`, with no `stop`. Sending `player/clear_queue` before each
   `play_stream` fixed it: the track played to the end, then a clean
   `state=stop`, and no other file was fetched. A later read-only check,
   with a stream paused, listed one entry: `returned=1&count=1`,
   song/album/artist "Url Stream", `qid` 1, empty `mid`. Queue entries for
   URL streams carry no URL, and their count did not match what the
   receiver replayed. Test:
   `play_album_sets_the_volume_then_streams_the_first_track`.
2. **`clear_queue` answers twice.** First `"message": "command under
   process&pid=…"`, then the real reply about 0.13 s later. With an empty
   queue it fails: `eid=4`, text "Requested data not available". Tests:
   `an_empty_queue_does_not_stop_playback`,
   `events_and_under_process_notes_before_a_reply_are_skipped`.
3. **`state=unknown`.** `get_play_state` can answer `unknown`, before a
   stream starts and after it ends. kids-deck treats it as `stop`. Test:
   `unknown_counts_as_not_playing`.
4. **Retries after the last track.** Without a stop, the receiver retried
   the finished stream about every 3 s, with `event/player_playback_error`
   "Playback error. Could not decode the audio stream" and "Unsupported
   format", and the state flipping between `stop` and `unknown`. Once it
   played the last track again. So kids-deck sends
   `player/set_play_state?state=stop` when an album ends. Test:
   `the_last_track_ending_stops_the_album`.
5. **Track length and now playing.**
   `event/player_now_playing_progress` gave `duration=15000` for an MP3 and
   `duration=0` for an m4a whose index (moov atom) was at the end; progress
   events came every ~5 s. `get_now_playing_media` for a URL stream has no
   URL in it (`qid` was 2 during the tests, 1 in the later check):

   ```json
   {"heos": {"command": "player/get_now_playing_media",
             "result": "success", "message": "pid=-1234567890"},
    "payload": {"type": "song", "song": "Url Stream",
                "album": "Url Stream", "artist": "Url Stream",
                "image_url": "", "album_id": "1", "mid": "1", "qid": 1,
                "sid": 1024},
    "options": []}
   ```

6. **CLI hangs.** Twice the CLI stopped answering for about 2 minutes right
   after clients disconnected abruptly: ping worked, the TCP connect
   worked, no replies came. Then it reset the connections and recovered.
   kids-deck times out after 5 s and reports `Stopped`; three failed polls
   in a row end the album. The next command reconnects. A command that
   finds its connection reset is sent again on a new one; a timed-out
   command is not. Tests: `a_silent_cli_fails_within_the_io_timeout`,
   `a_timed_out_command_is_not_sent_again`,
   `a_dropped_connection_is_reopened_and_the_command_sent_again`,
   `a_timed_out_clear_queue_stops_the_track_change`.
7. **`get_players`.** `pid` is a negative JSON number (reply above).
   `lineout` is `0`, which is not in the spec's list (1 variable, 2 fixed).
   Test: `players_lists_names_pids_and_ips`.
8. **macOS Local Network privacy.** A self-built binary gets "No route to
   host" to LAN devices while `ping` and `nc` work. Docker is not affected.
   Reproduced on 2026-09-24: the `kids-deck --check` binary printed
   `NOT reachable: … No route to host (os error 65)` while
   `/usr/bin/python3` got the reply above. Grant the terminal app Local
   Network access, or use `just sim`.

## Test by hand

`just doctor` connects, sends `get_players` and prints each player (the
format of `print_heos_players`, with the AVR-X1600H's values):

```text
Speaker 192.168.1.40:1255 (Heos):
  reachable ✓
  player Den (Denon AVR-X1600H), ip 192.168.1.40, pid -1234567890
```

To send one command and print the reply, save this as `heos.py` (Python 3,
standard library only):

```python
import json
import socket
import sys

host, command = sys.argv[1], sys.argv[2]
with socket.create_connection((host, 1255), timeout=5) as sock:
    sock.sendall(f"heos://{command}\r\n".encode())
    buf = b""
    while True:
        chunk = sock.recv(65536)
        if not chunk:
            sys.exit("connection closed")
        buf += chunk
        *lines, buf = buf.split(b"\r\n")
        for line in filter(None, lines):
            print(line.decode())
            reply = json.loads(line)["heos"]
            if "under process" not in reply.get("message", ""):
                sys.exit()
```

```sh
python3 heos.py 192.168.1.40 player/get_players
python3 heos.py 192.168.1.40 "player/get_play_state?pid=-1234567890"
```

It prints the "command under process" line too, and stops at the real
reply. Change events are off on a new connection, so no event line comes
in between. The `with` block closes the socket cleanly; abrupt disconnects came
before the CLI hangs (item 6). `play_stream`, `set_play_state` and
`clear_queue` change what the receiver plays.

To see which files the receiver fetches, run with the HTTP trace:

```sh
RUST_LOG=info,tower_http=debug just sim   # in Docker
just debug                                 # native
```

Each file request gives two lines (timestamps left out, wrapped):

```text
DEBUG request{method=GET uri=/music/01%20High%20Tones/01%20Tone%20880%20Hz.m4a
    version=HTTP/1.1}: tower_http::trace::on_request: started processing
    request
DEBUG request{…}: tower_http::trace::on_response: finished processing
    request latency=0 ms status=200
```

After each `playing album=… track=…` line, expect one `GET` of that track
(`status=200`, or `206` for a range request). A `GET` of a file that is not
the current track means the receiver is playing a stale stream (item 1).

## References

- [HEOS CLI Protocol Specification v1.17][spec] (PDF): §2 Connection,
  §2.1.1 Driver Initialization, §3 Command and Response Overview, §4.1.1,
  §4.2.1, §4.2.3, §4.2.4, §4.2.7, §4.2.19, §4.4.10 Play URL, §5 Change
  Events, §6 Error Codes.
- [pyheos `message.py`][pyheos] (Home Assistant's library): encodes
  `&`, `=`, `%` as `%26`, `%3D`, `%25` and appends `url` last, unencoded.
- Code: [`heos.rs`](../src/player/heos.rs),
  [`heos/cli.rs`](../src/player/heos/cli.rs),
  [`heos/tests.rs`](../src/player/heos/tests.rs),
  [`mod.rs`](../src/player/mod.rs), [`config.rs`](../src/config/mod.rs),
  `print_heos_players` in [`main.rs`](../src/main.rs).

[spec]: https://rn.dmglobal.com/usmodel/HEOS_CLI_ProtocolSpecification-Version-1.17.pdf
[pyheos]: https://raw.githubusercontent.com/andrewsayre/pyheos/master/pyheos/message.py
