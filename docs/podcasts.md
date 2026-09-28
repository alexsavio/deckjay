# Podcasts

A `[[source]]` with `type = "podcast"` is one shelf of podcast episodes.
deckjay reads the feeds, downloads the newest episodes into a cache
folder, and plays them like any other file: the speaker fetches them from
the deckjay web server, or local audio plays them from the disk. The
code:

- [`src/config/podcast.rs`](../src/config/podcast.rs): the source table and
  its checks.
- [`src/podcasts/`](../src/podcasts/mod.rs): the `podcasts` thread: feed
  reading (`feed.rs`), what to keep (`plan.rs`, no IO), the cache and its
  manifest (`cache.rs`), downloads (`download.rs`) and the refresh loop
  (`refresh.rs`).
- [`src/library/podcast.rs`](../src/library/podcast.rs): the thread's
  settings from the config, and episodes as deck items.
- [`src/ui/podcasts.rs`](../src/ui/podcasts.rs): new snapshots on the deck,
  and the pin on the playing episode.

## Configuration

```toml
[[source]]
type = "podcast"
name = "maus"            # the shelf; default "podcast"
keep = 3                 # newest episodes per feed (default 5, at most 50)
refresh_hours = 6        # default 6, 1 to 168
max_cache_mb = 4000      # for all feeds of this source
max_episode_mb = 500     # bigger episodes are skipped
# cache_dir = "..."      # default <state_dir>/podcasts/<name>
[[source.feed]]
name = "Die Maus"
url = "https://example.org/maus.xml"
# order = "oldest"       # for stories in parts; default "newest"
# keep = 10              # this feed only
```

All feeds of a source share one shelf, feed after feed. For one shelf per
podcast, write one source per podcast. Each feed gets a folder in the
cache, named after the feed's `name` (lower case, `-` for other
characters): renaming a feed downloads it again into a new folder.

`just doctor` reads every feed once and prints its title and number of
episodes, or why it cannot be read.

## How the cache works

Each podcast source runs one `podcasts` thread. At start it serves what the
cache already holds (the deck works offline), then it refreshes: at once,
then every `refresh_hours`. When every feed of the source fails, it tries
again after 1 minute, doubling up to `refresh_hours`.

A refresh reads each feed, keeps the newest `keep` episodes that fit in
`max_cache_mb` (shared between the feeds in turn), downloads the missing
ones, and writes `feed.json` in the feed's folder. Downloads go to a `.part`
file first and continue with an HTTP range after a break. A feed that
cannot be read keeps all its cached episodes, even over the budget, so a
broken feed never empties the shelf. Nothing in a feed's folder is deleted
while the feed or its `feed.json` cannot be read. A broken `feed.json`
takes the feed's episodes off the shelf until a refresh reads the feed and
writes it again, reusing the episode files already there.

Episodes that fall out of the plan are deleted 60 seconds later; then the
web server stops serving them. The episode that is loaded (playing or
paused) is never deleted, even when a refresh takes it off the shelf: the
deck tells the thread which one it is, and the thread keeps it until it
stops or another item starts.

The cache folder of a source must be writable. Folders in it that belong
to no configured feed are left alone and logged.

## On the deck

- Each episode is one key: its picture, else the feed's picture, else the
  microphone glyph. With shelves of more than one kind, pictures get a
  microphone badge.
- A red dot marks an episode that has never played.
- Episodes resume where they stopped, like audiobooks (saved in
  `state.json` by the key `<source>/<feed folder>/<episode id>`), and the
  key shows a progress bar.
- A refresh refills the shelf while the deck runs. Known episodes keep
  their place in memory, so progress and cached key images stay valid.

## Limits

- Only audio enclosures with a known type play: MP3, M4A/AAC, OGG, OPUS,
  FLAC, WAV. Video episodes are skipped.
- Local audio cannot decode Opus or HE-AAC; the speaker may.
- Private feeds with a token in the URL work; logs leave the query out.
