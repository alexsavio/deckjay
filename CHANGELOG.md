# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Calendar Versioning](https://calver.org/) (YYYY.MM.PATCH).

## [Unreleased]

### 🐛 Bug Fixes

- *(test)* Raise the open-file limit for the test runs ([#12](https://github.com/alexsavio/deckjay/issues/12))
- *(release)* Stop when main moves during a release ([#13](https://github.com/alexsavio/deckjay/issues/13))

## [2026.9.1] - 2026-09-29

### 🚀 Features

- *(cli)* Commands, check exit codes, --version and systemd notify ([#10](https://github.com/alexsavio/deckjay/issues/10))

### 📚 Documentation

- *(pi)* Drop the upgrade section ([#9](https://github.com/alexsavio/deckjay/issues/9))
- *(readme)* Add the crates.io badge ([#11](https://github.com/alexsavio/deckjay/issues/11))

## [2026.9.0] - 2026-09-28

### 🚀 Features

- Kids music deck player
- Web Stream Deck simulator
- Play albums on Denon HEOS speakers
- Play albums on the computer's own sound output
- Enable https for outgoing requests
- [**breaking**] Read what to play from [[source]] tables
- *(spotify)* Sign-in, tokens and Web API client
- *(podcasts)* Feeds, download plan, cache and refresh thread
- *(icons)* Add key decorations, kind glyphs and shelf/flip keys
- Show sources as shelves and remember where the deck was
- Sign in to Spotify and check its devices
- Add podcast sources
- Add radio sources
- Add Spotify playlist sources
- Resolve radio playlists to their streams
- Report playback progress and start at a given track
- Save how far audiobooks and episodes got
- Resume inside a track
- Play Spotify playlists on a Connect device
- Play internet radio on HEOS
- Play internet radio on Chromecast
- Play internet radio on local audio
- Stop a speaker before another takes over
- Add a power key that stops everything and dims the deck
- Let a source folder be one key
- Jump back and forward in audiobooks and podcasts
- Turn the deck dark when kids-deck stops
- Make the deck simulator look like a real Stream Deck
- *(deck)* Log key presses, speaker events and deck errors with context ([#3](https://github.com/alexsavio/deckjay/issues/3))

### 🐛 Bug Fixes

- Report Stopped after every failed speaker command
- *(icons)* Draw the Spotify glyph as concentric arcs, not stacked ones
- *(icons)* Keep a shelf key's paging dots off the picture
- End a book on Cast when its session ends near the last track's end
- Keep a playing episode pinned and podcast tiles current
- Show stations and playlists in the check and logs
- Let the power key stop what kids-deck did not start
- Let the rewind and forward keys jump in stories too
- *(release)* Let git-cliff print release notes and previews ([#4](https://github.com/alexsavio/deckjay/issues/4))
- *(changelog)* Link the PR numbers in commit subjects ([#6](https://github.com/alexsavio/deckjay/issues/6))
- *(ci)* Regenerate the changelog on the newest main instead of rebasing ([#7](https://github.com/alexsavio/deckjay/issues/7))

### 💼 Other

- Release CalVer versions to GitHub and crates.io
- Harden the player against hangs, dead threads and podcast data loss ([#1](https://github.com/alexsavio/deckjay/issues/1))
- fix(player): give failing polls 20 s and keep the album on a failed volume or skip

The failure budget counted polls, so HEOS (1 s poll) gave a network blip
2 s before it ended the album while the receiver played on, and Cast 12 s.
Every failed command reset the album, volume, skip and seek included,
which the UI never guesses on; the polls now find out whether the speaker
is gone. A panic in symphonia while opening a file or stream is caught and
skips the track, so one corrupt file cannot end the player thread.

- fix(cast): own the speaker socket so every read and write has a timeout

rust_cast opens its socket with a bare connect and sets no timeout, so a
Chromecast that accepted the connection and then went quiet held the
player thread until a restart, while the deck looked alive. kids-deck
now connects itself (3 s), sets 15 s read and write timeouts, wraps the
socket in TLS without a certificate check as before, and hands it to the
rust_cast channels. The same seam runs the tests against a fake speaker
over plain TCP, the first tests of the Cast network path.

- fix(ui): exit when the player thread ends, and find the speaker route at each album

The UI drained events with try_iter, which cannot tell an empty channel
from one whose sender is gone, so a panic on the player thread left a
deck that drew but played nothing, and systemd never restarted it. The
UI now returns PlayerGone and main exits with it.

The address the speaker downloads from was found once at start, so a
start with the speaker off or DNS down exited, and a new DHCP address
broke every URL until a restart. net::BaseUrl finds it at each press,
resolves the speaker's name once, and starts the deck with a warning
when there is no route yet.

- fix(library): play tracks in natural order, a folder's own files first

Names sorted byte by byte, so Kapitel 10 played before Kapitel 2 and Zebra
before apple, and whole paths compare one component at a time, so an
audiobook's Prolog.mp3 played after its CD1/ and CD2/ folders.

- fix(podcasts): cap decoded feed and picture bytes, not only wire bytes

ureq's body limit counts the bytes before gzip decoding, so a 100 KB gzip
feed or picture that expands to gigabytes was read into memory whole and
could kill a 1 GB Raspberry Pi at every start.

- fix(podcasts): keep a feed's cache when its manifest cannot be read

An unreadable feed.json counted as an empty cache, so when the feed fetch
failed too, every episode and picture in the folder was swept 60 s later.
A refresh that cannot read the feed or its manifest now schedules no
deletions for that feed; its pending ones wait for a refresh with both.

- fix(radio): bound the playlist body read so a stalled server cannot hang the player

A playlist server that sends its headers and then stalls holds the player
thread, and with it every command including the power key, until a
restart. The resolve request gets a 10 s body budget of its own; the
stream agent keeps none, since a stream's reply is dropped after its head.

- fix(local): stop probing a silent station after a few seconds and back off reconnects

A station that sends headers and no body holds the player thread for 40 s
of idle waits and reconnects, and leaves four reader threads blocked for
good; a failed reconnect retries at once, so a short outage uses up all
three attempts. Opening waits 5 s for the first bytes and then fails
without reconnecting, a connection that sent nothing is followed by a
growing wait, and a 3 h body budget frees a reader stuck on a stalled
connection.

- docs: name BaseUrl in the code map entry for net.rs

- docs: drop comments that restate the code next to them

- fix(local): keep the decodable doc on decodable

guarded was inserted below the /// symphonia has no HE-AAC (SBR) and
no HLS. line, which documents decodable, not guarded. Move guarded
above it so the doc sits on decodable again.

- Show why a press failed on the simulator page and mark the key ([#2](https://github.com/alexsavio/deckjay/issues/2))

- feat(simulator): show why a press failed on the page and mark the key

A failed press left the deck showing "stopped" and the page saying
"Player connected", with the reason only in the player's log. The player
now follows the Stopped with PlayerEvent::Trouble(text); the UI sends the
text to the deck backend, which the simulator shows under the keys until
something plays again, and the item pressed last wears a red mark with an
exclamation mark for 4 s, on the real deck too.

- test(player): check the trouble event when the speaker stops answering

polls_failing_for_the_whole_grace_stop_the_album now asserts the
Trouble event that follows Stopped when the speaker gives up on polls,
matching the assertion already made for the command-failure branch.

- docs: drop comments that restate the code next to them

### 🚜 Refactor

- Apply code review findings
- Split the ui module into layout and tests files
- Split the library into a module and its folder scan
- Move the HEOS CLI protocol into heos/cli.rs
- Name library items by stable ids and keys
- Play an item's content instead of an album
- Let the deck forget images of faces it no longer shows
- Let a speaker stop before another one takes over
- [**breaking**] Rename the project to deckjay ([#5](https://github.com/alexsavio/deckjay/issues/5))

### 📚 Documentation

- Describe the HEOS and Cast protocols kids-deck uses
- Record Chromecast testing on a Lenovo smart display
- Describe items, shelves and the Play command
- Describe radio, Spotify and progress in the architecture notes
- Explain podcast sources and Spotify
- Map the router, Spotify, podcasts and radio modules
- Describe radio playback in the architecture notes
- Say which Spotify devices stay reachable
- Add an MIT license and a README for the public
- Add development and Raspberry Pi guides
- Play on a Bluetooth speaker and Spotify on the Pi
- Rename the repository to kids-deck and say it runs on Linux
- List the supported decks and split the README into short parts
- *(pi)* Add Raspberry Pi Zero 2 W notes

### ⚙️ Miscellaneous Tasks

- Add empty spotify and podcasts modules
- Let just sim mount source folders from a local compose file
- Add CI, zizmor and changelog workflows
- Let each job read the repository
- Check Markdown with the rumdl version used locally
- Retry the changelog push and name why jobs read the repo
- *(cargo)* Keep dev tooling out of the crate ([#8](https://github.com/alexsavio/deckjay/issues/8))
<!-- generated by git-cliff -->
