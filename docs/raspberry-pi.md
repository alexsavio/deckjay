# 🍓 Run on a Raspberry Pi

This guide installs kids-deck on a Raspberry Pi with no screen and no
keyboard ("headless"), on Raspberry Pi OS (Raspbian). kids-deck runs as a
systemd service: it starts at boot, starts again when it fails, and writes
its logs to the system journal. You do every step over SSH from your
computer.

The Pi keeps the music on its SD card. It plays on a Chromecast, on a HEOS
receiver, or on its own sound output (headphone jack, HDMI or a USB sound
card).

## 🧰 What you need

- A **Raspberry Pi 3** or newer, with a power supply and a microSD card
  (16 GB or more, plus the size of your music).
- **64-bit Raspberry Pi OS Lite**, trixie (Debian 13) or bookworm
  (Debian 12). The release binaries do not run on the 32-bit OS.
- The Stream Deck, plugged into the Pi.
- A computer on the same network, with SSH and `rsync`.

## 💾 1. Prepare the SD card

Write the card with
[Raspberry Pi Imager](https://www.raspberrypi.com/software/). Pick
**Raspberry Pi OS Lite (64-bit)** ("Raspberry Pi OS (other)"), then open
the settings (OS customisation) before you write:

- Hostname: `kidsdeck`.
- A user name and a password of your choice. This guide calls the user
  `you`.
- Wi-Fi name and password, if the Pi has no network cable.
- Services: turn on SSH, with public-key authentication, and paste your
  computer's public key (`~/.ssh/id_ed25519.pub`).

Put the card in the Pi and power it on. The first boot takes a few
minutes. Then, from your computer:

```sh
ssh you@kidsdeck.local
```

Check that the Pi runs the 64-bit OS; the first command must print
`aarch64`:

```sh
uname -m
grep VERSION_CODENAME /etc/os-release
```

## 📚 2. Install the libraries

On the Pi:

```sh
sudo apt update
sudo apt install -y libudev1 libasound2t64 ca-certificates rsync
```

On bookworm, the sound library is `libasound2` in place of
`libasound2t64`.

## 📦 3. Install kids-deck

Download the arm64 archive of the newest release, check it, and install the
program:

```sh
base=https://github.com/alexsavio/kids-music-deck/releases/latest/download
curl -fsSLO "$base/kids-deck-aarch64-unknown-linux-gnu.tar.gz"
curl -fsSLO "$base/SHA256SUMS.txt"
sha256sum --check --ignore-missing SHA256SUMS.txt
tar -xzf kids-deck-aarch64-unknown-linux-gnu.tar.gz
cd kids-deck-aarch64-unknown-linux-gnu
sudo install -m 0755 kids-deck /usr/local/bin/kids-deck
kids-deck --help
```

The archive also holds `config.example.toml` and `99-streamdeck.rules`;
the next steps use them from this folder. If the releases page has no
release yet, or to build the program yourself, see
[Build the binary yourself](#-build-the-binary-yourself).

## 👤 4. Make a user for the service

The service runs as its own system user, `kidsdeck`, not as root. The
`plugdev` group lets it use the Stream Deck (with the udev rule below), and
the `audio` group lets it play on the Pi's sound output.

```sh
sudo useradd --system --user-group --no-create-home \
  --home-dir /var/lib/kids-deck --shell /usr/sbin/nologin \
  --groups plugdev,audio kidsdeck
```

Install the udev rule, which gives the `plugdev` group access to Elgato
devices:

```sh
sudo install -m 0644 99-streamdeck.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=usb --subsystem-match=hidraw
```

## 🎵 5. Copy the music

The music goes to `/srv/kids-deck`, one folder per source. Make the folder
yours, so you can copy to it without `sudo`. On the Pi:

```sh
sudo install -d -o "$USER" -g "$USER" -m 0755 /srv/kids-deck
```

Then, on your computer, copy each music folder:

```sh
rsync -rtLP --chmod=D755,F644 "$HOME/Music/Kids Music/" you@kidsdeck.local:/srv/kids-deck/music/
rsync -rtLP --chmod=D755,F644 "$HOME/Music/Kids Stories/" you@kidsdeck.local:/srv/kids-deck/stories/
```

- The `/` at the end of the source copies what is in the folder, not the
  folder itself.
- `-L` copies the files that symlinks point to. A music folder that is a
  folder of symlinks to albums works too.
- `--chmod` makes every file readable by the `kidsdeck` user.
- rsync only adds and updates files; it never deletes on the Pi. Add
  `--delete` to also remove what is no longer on your computer.

Run the same commands again when the music changes.

## 📝 6. Write the configuration

The configuration goes to `/etc/kids-deck/config.toml`. Only root can
change it, and only the `kidsdeck` user can read it (it can hold a
Spotify client id):

```sh
sudo install -d -o root -g kidsdeck -m 0750 /etc/kids-deck
sudo install -o root -g kidsdeck -m 0640 config.example.toml /etc/kids-deck/config.toml
sudo nano /etc/kids-deck/config.toml
```

`config.example.toml` explains every key. On the Pi, use absolute paths:
relative ones start from `/etc/kids-deck`, where the service cannot
write. An example that plays on the headphone jack:

```toml
speaker_type = "local"
audio_device = "Headphones"
max_volume = 0.4
state_dir = "/var/lib/kids-deck"

[[source]]
type = "music"
path = "/srv/kids-deck/music"

[[source]]
type = "story"
path = "/srv/kids-deck/stories"
```

- For a Chromecast or a HEOS receiver, set `speaker_type` to `"cast"` or
  `"heos"` and `speaker_host` to the speaker's IP address. Give the
  speaker a fixed IP in your router.
- `state_dir` is where the deck remembers the shelf, the page and where
  each audiobook stopped. It also holds the podcast downloads and the
  Spotify sign-in. Podcast sources keep up to 4000 MB each by default: set
  `max_cache_mb` lower on a small SD card.
- The names of the Pi's sound outputs are in
  [local-audio.md](local-audio.md#raspberry-pi-3).
- Keep the top-level keys above the first `[[source]]`: TOML reads every
  key after a table header as part of that table.

Check the configuration before you start the service. `--check` lists the
sources and their items, the Stream Decks, and the sound outputs (local
audio) or whether the speaker answers:

```sh
sudo -u kidsdeck kids-deck --check /etc/kids-deck/config.toml
```

## 🚀 7. Start it at boot

Write the service file:

```sh
sudo tee /etc/systemd/system/kids-deck.service >/dev/null <<'EOF'
[Unit]
Description=kids-deck Stream Deck music player
Documentation=https://github.com/alexsavio/kids-music-deck
# The speaker needs our LAN address, found from the route to it.
Wants=network-online.target
After=network-online.target sound.target
# Never give up: the speaker may be off at boot, the deck unplugged.
StartLimitIntervalSec=0

[Service]
Type=simple
User=kidsdeck
Group=kidsdeck
SupplementaryGroups=plugdev audio
# Creates /var/lib/kids-deck, owned by kidsdeck.
StateDirectory=kids-deck
WorkingDirectory=/var/lib/kids-deck
ExecStart=/usr/local/bin/kids-deck /etc/kids-deck/config.toml
Restart=always
RestartSec=5
# Wait up to a minute between starts when it keeps failing (systemd 254 or
# newer; bookworm's systemd ignores these two lines).
RestartSteps=5
RestartMaxDelaySec=60
# No colour codes in the journal.
Environment=NO_COLOR=1
Environment=RUST_BACKTRACE=1

# Light hardening. The deck (hidraw, usb) and the sound card (/dev/snd)
# must stay visible, and replugging the deck must work.
NoNewPrivileges=yes
ProtectSystem=full
ProtectHome=read-only
PrivateTmp=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes

[Install]
WantedBy=multi-user.target
EOF
```

Then start it, now and at every boot:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now kids-deck
systemctl status kids-deck
```

The deck lights up with the covers. Unplug and plug the deck, or turn the
speaker off and on: the program waits for them and goes on.

## 📜 8. Keep the logs

The service writes to the system journal. Keep the journal on the SD card
across reboots, but cap its size, so it rotates and never fills the card:

```sh
sudo install -d /etc/systemd/journald.conf.d
sudo tee /etc/systemd/journald.conf.d/50-kids-deck.conf >/dev/null <<'EOF'
[Journal]
Storage=persistent
Compress=yes
# All journal files together; the oldest go first.
SystemMaxUse=200M
# One file rotates at this size.
SystemMaxFileSize=20M
# The journal in RAM, used before /var is mounted.
RuntimeMaxUse=50M
MaxRetentionSec=1month
EOF
sudo systemd-tmpfiles --create --prefix /var/log/journal
sudo systemctl restart systemd-journald
```

This setting is for the whole Pi, not only for kids-deck.

## 🔍 Read the logs

On the Pi:

```sh
journalctl -u kids-deck -f                    # follow
journalctl -u kids-deck -b -1                 # the boot before this one
journalctl -u kids-deck --since "1 hour ago"
journalctl --disk-usage
```

From your computer, in one command:

```sh
ssh -t you@kidsdeck.local journalctl -u kids-deck -f
```

For more detail, such as every file a speaker downloads, run
`sudo systemctl edit kids-deck` and add:

```ini
[Service]
Environment=RUST_LOG=debug,tower_http=debug
```

Then `sudo systemctl restart kids-deck`. `sudo systemctl revert kids-deck`
removes the change again.

## 🔁 Day to day

- **New music:** copy it with the `rsync` commands of step 5, then
  `sudo systemctl restart kids-deck`. The program reads the music folders
  when it starts.
- **A new version:** download and install it as in step 3, then
  `sudo systemctl restart kids-deck`.
- **Check after a change:** stop the service first, because it holds the
  deck:

  ```sh
  sudo systemctl stop kids-deck
  sudo -u kidsdeck kids-deck --check /etc/kids-deck/config.toml
  sudo systemctl start kids-deck
  ```

- **Spotify:** set up the Spotify app and `[spotify]` as in
  [spotify.md](spotify.md), start the service once (it makes
  `/var/lib/kids-deck`), then sign in and restart:

  ```sh
  sudo -u kidsdeck kids-deck spotify-login /etc/kids-deck/config.toml
  sudo systemctl restart kids-deck
  ```

  The Pi has no browser: open the address it prints on your computer, and
  paste the address the browser ends up at back into the terminal. Or
  connect with `ssh -L 8898:127.0.0.1:8898 you@kidsdeck.local` first, and
  the sign-in finishes by itself.
- **Firewall:** kids-deck needs no firewall change. If you turn on `ufw`,
  allow SSH (22/tcp) and, for a Chromecast or HEOS speaker, the music
  server (8765/tcp, `http_port`): the speaker downloads the music from it.

## 🔵 Play on a Bluetooth speaker

> **Not tested yet:** a real Bluetooth speaker, and raspotify together with
> kids-deck. These steps follow the Debian packages, their manuals and a
> test in a Debian container. One risk is known: after a playlist, the
> first press on a book may fail because Spotify still holds the speaker.
> The next press should play (see the end of
> [Spotify on the Pi itself](#-spotify-on-the-pi-itself)).

To kids-deck a paired Bluetooth speaker is one more sound output, so it
plays with `speaker_type = "local"`. (On a Mac: pair the speaker in System
Settings, then set `audio_device` to part of its name; `--check` lists the
names.)

Raspberry Pi OS Lite has no sound server, and PipeWire runs only in a
user's login session, which the `kidsdeck` service user never has.
`bluez-alsa` works without one: a system service that gives every member
of the `audio` group an ALSA device named `bluealsa`.

1. Install it. Debian's service sends audio (`a2dp-source`) with no
   changes:

   ```sh
   sudo apt install -y bluez bluez-alsa-utils libasound2-plugin-bluez
   sudo systemctl enable --now bluealsa
   ```

2. Pair the speaker. Put it in pairing mode, then:

   ```sh
   sudo bluetoothctl
   scan on                 # wait for the speaker's name and address
   pair XX:XX:XX:XX:XX:XX
   trust XX:XX:XX:XX:XX:XX # it may connect again by itself later
   connect XX:XX:XX:XX:XX:XX
   exit
   ```

3. Make the speaker the Pi's default sound output, for kids-deck and every
   other program:

   ```sh
   sudo tee /etc/asound.conf >/dev/null <<'EOF'
   defaults.bluealsa.device "XX:XX:XX:XX:XX:XX"
   pcm.!default {
       type plug
       slave.pcm "bluealsa"
   }
   EOF
   ```

4. Leave `audio_device` out of `/etc/kids-deck/config.toml` (it plays on
   the default output), check, and restart:

   ```sh
   sudo systemctl stop kids-deck
   sudo -u kidsdeck kids-deck --check /etc/kids-deck/config.toml
   speaker-test -D default -c 2 -t sine -l 1   # a short tone on the speaker
   sudo systemctl start kids-deck
   ```

Things to know:

- Many Bluetooth speakers turn off after 10 to 20 minutes of silence.
  Turned on again, most connect to the Pi by themselves (it is trusted);
  if one does not, run `sudo bluetoothctl connect XX:XX:XX:XX:XX:XX`.
  While the speaker is off, a press gives no sound.
- On a Raspberry Pi 3, Bluetooth and Wi-Fi share one radio chip, and
  Bluetooth audio can stutter while Wi-Fi is busy. Use a network cable, or
  a USB Bluetooth adapter.
- `max_volume` caps the deck's keys only; the speaker's own buttons can
  still go louder.

## 🟢 Spotify on the Pi itself

A Bluetooth speaker has no Spotify Connect, so the Pi becomes the Spotify
Connect device: [raspotify](https://github.com/dtcooper/raspotify) runs
librespot as a service and plays on the Pi's default output, the
Bluetooth speaker above. Unlike a Chromecast, it stays in Spotify's device
list. Not tested with kids-deck yet (see the note at the top of
[Play on a Bluetooth speaker](#-play-on-a-bluetooth-speaker)).

1. Install raspotify (it has arm64 packages):

   ```sh
   sudo apt install -y curl
   curl -sL https://dtcooper.github.io/raspotify/install.sh | sh
   ```

2. In `/etc/raspotify/conf`, set the name, and let it keep the Spotify
   sign-in, so the Pi stays in your account's device list after a reboot:

   ```sh
   LIBRESPOT_NAME="kidsdeck"
   #LIBRESPOT_DISABLE_CREDENTIAL_CACHE=
   ```

   (Put a `#` in front of the `LIBRESPOT_DISABLE_CREDENTIAL_CACHE=` line.)
   Then `sudo systemctl restart raspotify`.

3. Sign it in once: on a phone on the same network, open Spotify, tap the
   devices icon and pick **kidsdeck**. It keeps the sign-in in
   `/var/lib/raspotify`.

4. In `/etc/kids-deck/config.toml`, set the device, and sign kids-deck in
   to Spotify as in [Day to day](#-day-to-day):

   ```toml
   [spotify]
   client_id = "..."
   device = "kidsdeck"
   ```

   `--check` lists the Spotify devices your account sees; `kidsdeck` must
   be one of them.

kids-deck closes its sound output when a playlist starts, and pauses
Spotify when an album, a book or a station starts. librespot should free
the output when it pauses, but the pause reaches the Pi through Spotify's
servers and can take a moment: the first press after a playlist may fail,
and the next one play, because `bluez-alsa` may not let two programs use
the speaker at once.

## 🧱 Build the binary yourself

Build it on your computer with Docker; a Pi 3 has too little memory to
build it. The image builds on Debian trixie, so this binary needs Raspberry
Pi OS trixie. In a checkout of this repository:

```sh
just image
id=$(docker create --platform linux/arm64 kids-deck)
docker cp "$id":/usr/local/bin/kids-deck ./kids-deck
docker rm "$id"
scp kids-deck config.example.toml 99-streamdeck.rules you@kidsdeck.local:
```

Then go on with step 3 from `sudo install`, in your home folder on the Pi.

## 🐳 Run it in Docker instead

The repository can also run kids-deck on the Pi in a Docker container. Put
Docker on the Pi; then on your computer, in a checkout of this
repository:

```sh
just deploy
just pi-logs
```

`just deploy` builds the arm64 image, copies it to the Pi with
`docker save | ssh docker load`, copies `docker-compose.yml`, `config.toml`
and `music/` to `~/kids-deck`, and runs `docker compose up -d` there. It
uses `pi@raspberrypi.local`; set `PI_HOST` (and `PI_DIR`) in `.env` to
change it.

Sources outside `music/` are not copied: put them on the Pi (or a drive
mounted there) at the path in `config.toml`, and add a volume for each in
`docker-compose.yml`.

`just deploy` only adds and updates music on the Pi; it never deletes. To
remove albums from the Pi that you deleted on your computer, run
`just pi-music-prune` (it asks first). `just pi-spotify-login` signs in to
Spotify on the Pi.

The container restarts after reboots and keeps looking for the Stream
Deck, so it can be unplugged and plugged back in.
