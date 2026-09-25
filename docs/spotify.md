# Spotify

kids-deck does not play Spotify audio itself. It remote-controls a Spotify
Connect device (a speaker, a receiver such as the Denon, or a Chromecast)
through the official Spotify Web API. That needs a Spotify **Premium**
account. The code:

- [`src/spotify/`](../src/spotify/mod.rs): the sign-in (`auth.rs`,
  `login.rs`), the saved login (`token.rs`) and the Web API client
  (`api.rs`).
- [`src/config/playlist.rs`](../src/config/playlist.rs): `type = "spotify"`
  sources and their playlists.

## 1. Make a Spotify app (once)

1. Open <https://developer.spotify.com/dashboard> and sign in with the
   Premium account.
2. Create an app. Name and description: anything, e.g. "kids-deck".
3. Redirect URI: `http://127.0.0.1:8898/callback`, exactly. Spotify refuses
   `localhost`.
4. APIs: tick **Web API**.
5. Copy the app's **Client ID**. The app never needs its client secret:
   kids-deck signs in with PKCE.

Apps stay in development mode: only the owner and up to five users added in
the dashboard can use them. That is enough for one family.

## 2. Configure

```toml
[spotify]
client_id = "0123456789abcdef0123456789abcdef"
device = "Denon"          # part of the Connect device's name

[[source]]
type = "spotify"
[[source.playlist]]
name = "Bedtime"
uri = "https://open.spotify.com/playlist/37i9dQZF1DX0XUsuxWHRQd"
```

`uri` takes a `spotify:playlist:...` URI or a share link from the app
(Share, Copy link). Each playlist is one key.

## 3. Sign in (once)

```sh
just spotify-login        # on the Mac
just pi-spotify-login     # on the Pi, over SSH
```

The command prints an address. Open it in a browser, sign in and allow
kids-deck. The browser then goes to `http://127.0.0.1:8898/callback`:

- On the same computer, kids-deck answers there and the sign-in ends.
- On the Pi, that page does not load on the Mac's browser. Copy the
  address from the browser's address bar and paste it into the terminal.
  (Or forward the port: `ssh -L 8898:127.0.0.1:8898 pi@raspberrypi.local`.)

The login is saved in `<state_dir>/spotify-token.json` (mode 0600): the
client id and the refresh token, never printed. Access tokens stay in
memory and are refreshed when they expire. If Spotify stops accepting the
login, the log says to run `spotify-login` again.

Scopes: `user-read-playback-state`, `user-modify-playback-state`,
`playlist-read-private` and `playlist-read-collaborative`.

## 4. Check

`just doctor` shows the account, the Connect devices Spotify sees now, and
which one `device` picks: a device whose name equals `device` (ignoring
case) wins, else the first whose name contains it. Devices that the Web
API cannot control are listed but never picked.

A device appears only while it is awake and signed in to Spotify Connect.
A receiver in standby or a Chromecast that nobody cast to recently may be
missing: play something on it once from the Spotify app.

## How a playlist plays

[`src/player/router.rs`](../src/player/router.rs) sends a playlist press to
the Spotify player ([`src/player/spotify.rs`](../src/player/spotify.rs)) and
every other press to the configured speaker. When a press moves from one
to the other, the one that played stops first (Spotify gets a pause): the
speaker and the Connect device can be two different boxes.

A playlist press:

1. lists the Connect devices and picks `spotify.device`;
2. moves playback to that device if it is not the active one;
3. sets the device volume to the deck's volume, if the device lets the Web
   API set it;
4. plays the playlist from its first track. If Spotify answers that the
   device is idle, the transfer and the play are sent once more.

The deck's keys then work on Spotify too: play/pause, next, previous and
the volume keys (`max_volume` holds as for the speaker). kids-deck asks
Spotify what plays every 5 s while playing and every 15 s while paused. If
the device plays something else (someone took over in the Spotify app),
the key shows the playlist stopped.

## Covers

Each playlist key shows the playlist's cover once kids-deck has fetched
it: at start, a background thread saves missing covers in
`<state_dir>/spotify-covers/`. A new playlist's cover shows from the next
start; until then its key shows the Spotify glyph. `picture` in the
`[[source.playlist]]` table replaces the cover.

## Pick a device that stays listed

Spotify lists a Google Cast device (a Chromecast, a Lenovo smart display,
a TV with Chromecast built-in) only while Spotify is the app that plays on
it. When kids-deck plays an album, a book, a podcast or radio on the same
Cast device, Spotify loses it, and the next playlist press fails with "no
Spotify Connect device matches" until someone casts Spotify to it again
from the phone. So for Spotify pick a device with Spotify Connect built in
that stays listed: the Denon or other HEOS receivers (with Network Control
"Always On"), speakers such as the JBL Authentics, or a TV or streaming box
that runs the Spotify app.

## Limits

- Tested with a real account on a Lenovo smart display (Chromecast
  built-in): sign-in, token refresh, the
  cover, play, next, pause, resume, and the switch to a Cast album. The
  Denon is not tested yet.
- When a playlist ends, Spotify may go on with similar songs (autoplay).
  Turn autoplay off in the Spotify app settings.
- Playlists that Spotify itself made may have no cover through the API.
- In development mode the app works for the owner and up to five users.
