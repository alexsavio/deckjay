# 🍓 Run on a Raspberry Pi

This guide installs deckjay on a Raspberry Pi with no screen and no
keyboard ("headless"), on Raspberry Pi OS (Raspbian). deckjay runs as a
systemd service: it starts at boot, starts again when it fails, and writes
its logs to the system journal. You do every step over SSH from your
computer.

The Pi keeps the music on its SD card. It plays on a Chromecast, on a HEOS
receiver, or on its own sound output (headphone jack, HDMI or a USB sound
card).

## 🧰 What you need

- A **Raspberry Pi 3** or newer, with a power supply and a microSD card
  (16 GB or more, plus the size of your music). A Pi Zero 2 W works too,
  with a few changes: see
  [Run on a Raspberry Pi Zero 2 W](#-run-on-a-raspberry-pi-zero-2-w).
- **64-bit Raspberry Pi OS Lite**, trixie (Debian 13) or bookworm
  (Debian 12). The release binaries do not run on the 32-bit OS.
- The Stream Deck, plugged into the Pi.
- A computer on the same network, with SSH and `rsync`.

## 💾 1. Prepare the SD card

Write the card with
[Raspberry Pi Imager](https://www.raspberrypi.com/software/). Pick
**Raspberry Pi OS Lite (64-bit)** ("Raspberry Pi OS (other)"), then open
the settings (OS customisation) before you write:

- Hostname: `deckjay`.
- A user name and a password of your choice. This guide calls the user
  `you`.
- Wi-Fi name and password, if the Pi has no network cable.
- Services: turn on SSH, with public-key authentication, and paste your
  computer's public key (`~/.ssh/id_ed25519.pub`).

Put the card in the Pi and power it on. The first boot takes a few
minutes. Then, from your computer:

```sh
ssh you@deckjay.local
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

## 📦 3. Install deckjay

Download the arm64 archive of the newest release, check it, and install the
program:

```sh
base=https://github.com/alexsavio/deckjay/releases/latest/download
curl -fsSLO "$base/deckjay-aarch64-unknown-linux-gnu.tar.gz"
curl -fsSLO "$base/SHA256SUMS.txt"
sha256sum --check --ignore-missing SHA256SUMS.txt
tar -xzf deckjay-aarch64-unknown-linux-gnu.tar.gz
cd deckjay-aarch64-unknown-linux-gnu
sudo install -m 0755 deckjay /usr/local/bin/deckjay
deckjay --version
```

The archive also holds `config.example.toml` and `99-streamdeck.rules`;
the next steps use them from this folder. If the releases page has no
release yet, or to build the program yourself, see
[Build the binary yourself](#-build-the-binary-yourself).

## 👤 4. Make a user for the service

The service runs as its own system user, `deckjay`, not as root. The
`plugdev` group lets it use the Stream Deck (with the udev rule below), and
the `audio` group lets it play on the Pi's sound output.

```sh
sudo useradd --system --user-group --no-create-home \
  --home-dir /var/lib/deckjay --shell /usr/sbin/nologin \
  --groups plugdev,audio deckjay
```

Install the udev rule, which gives the `plugdev` group access to Elgato
devices:

```sh
sudo install -m 0644 99-streamdeck.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=usb --subsystem-match=hidraw
```

## 🎵 5. Copy the music

The music goes to `/srv/deckjay`, one folder per source. Make the folder
yours, so you can copy to it without `sudo`. On the Pi:

```sh
sudo install -d -o "$USER" -g "$USER" -m 0755 /srv/deckjay
```

Then, on your computer, copy each music folder:

```sh
rsync -rtLP --chmod=D755,F644 "$HOME/Music/Kids Music/" you@deckjay.local:/srv/deckjay/music/
rsync -rtLP --chmod=D755,F644 "$HOME/Music/Kids Stories/" you@deckjay.local:/srv/deckjay/stories/
```

- The `/` at the end of the source copies what is in the folder, not the
  folder itself.
- `-L` copies the files that symlinks point to. A music folder that is a
  folder of symlinks to albums works too.
- `--chmod` makes every file readable by the `deckjay` user.
- rsync only adds and updates files; it never deletes on the Pi. Add
  `--delete` to also remove what is no longer on your computer.

Run the same commands again when the music changes.

## 📝 6. Write the configuration

The configuration goes to `/etc/deckjay/config.toml`. Only root can
change it, and only the `deckjay` user can read it (it can hold a
Spotify client id):

```sh
sudo install -d -o root -g deckjay -m 0750 /etc/deckjay
sudo install -o root -g deckjay -m 0640 config.example.toml /etc/deckjay/config.toml
sudo nano /etc/deckjay/config.toml
```

`config.example.toml` explains every key. On the Pi, use absolute paths:
relative ones start from `/etc/deckjay`, where the service cannot
write. An example that plays on the headphone jack:

```toml
speaker_type = "local"
audio_device = "Headphones"
max_volume = 0.4
state_dir = "/var/lib/deckjay"

[[source]]
type = "music"
path = "/srv/deckjay/music"

[[source]]
type = "story"
path = "/srv/deckjay/stories"
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

Check the configuration before you start the service. `check-config`
reads the file and nothing else. `check` also lists the sources and their
items, the Stream Decks, and the sound outputs (local audio) or whether the
speaker answers; it exits with 1 when it found a problem and with 2 when it
found warnings only, so a script can read the result:

```sh
sudo -u deckjay deckjay check-config /etc/deckjay/config.toml
sudo -u deckjay deckjay check /etc/deckjay/config.toml
```

## 🚀 7. Start it at boot

Write the service file:

```sh
sudo tee /etc/systemd/system/deckjay.service >/dev/null <<'EOF'
[Unit]
Description=deckjay Stream Deck music player
Documentation=https://github.com/alexsavio/deckjay
# The speaker needs our LAN address, found from the route to it.
Wants=network-online.target
After=network-online.target sound.target
# Never give up: the speaker may be off at boot, the deck unplugged.
StartLimitIntervalSec=0

[Service]
# deckjay tells systemd when it runs and sends a heartbeat; without one
# for a minute (a hung program) systemd restarts it.
Type=notify
WatchdogSec=60
# How long systemd waits for "running" (default 90 s): the library scan and
# the speaker's name lookup must fit, also on a slow card.
TimeoutStartSec=5min
User=deckjay
Group=deckjay
SupplementaryGroups=plugdev audio
# Creates /var/lib/deckjay, owned by deckjay.
StateDirectory=deckjay
WorkingDirectory=/var/lib/deckjay
ExecStart=/usr/local/bin/deckjay /etc/deckjay/config.toml
# Turns the deck dark however deckjay ended, a crash included.
ExecStopPost=/usr/local/bin/deckjay blank
Restart=always
RestartSec=5
# Wait up to a minute between starts when it keeps failing (systemd 254 or
# newer; bookworm's systemd ignores these two lines).
RestartSteps=5
RestartMaxDelaySec=60
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
sudo systemctl enable --now deckjay
systemctl status deckjay
```

The deck lights up with the covers. Unplug and plug the deck, or turn the
speaker off and on: the program waits for them and goes on.

When deckjay stops (`systemctl stop`, a shutdown), it turns the deck
dark itself. When it crashes or is killed (`kill -9`, out of memory), it
cannot, so the `ExecStopPost` line runs `deckjay blank` after it; 5 s
later systemd starts it again and the deck lights up. Unplugging the deck
or turning the Pi off also leaves it dark. When the main thread hangs (the
deck stops reacting to presses), its heartbeat stops, and systemd kills and
restarts the program after the minute of `WatchdogSec=`. A stuck player
thread is not caught this way: the deck reacts, but nothing plays. Then
`sudo systemctl restart deckjay`.

## 📜 8. Keep the logs

The service writes to the system journal. Keep the journal on the SD card
across reboots, but cap its size, so it rotates and never fills the card:

```sh
sudo install -d /etc/systemd/journald.conf.d
sudo tee /etc/systemd/journald.conf.d/50-deckjay.conf >/dev/null <<'EOF'
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

This setting is for the whole Pi, not only for deckjay.

## 🔍 Read the logs

On the Pi:

```sh
journalctl -u deckjay -f                    # follow
journalctl -u deckjay -b -1                 # the boot before this one
journalctl -u deckjay --since "1 hour ago"
journalctl --disk-usage
```

From your computer, in one command:

```sh
ssh -t you@deckjay.local journalctl -u deckjay -f
```

For more detail (each key press and speaker event, every file the speaker
downloads, the Elgato devices found, and the time tiles and redraws take),
run `sudo systemctl edit deckjay` and add:

```ini
[Service]
Environment=RUST_LOG=debug,tower_http=debug
```

Then `sudo systemctl restart deckjay`. `sudo systemctl revert deckjay`
removes the change again.

## 🔁 Day to day

- **New music:** copy it with the `rsync` commands of step 5, then
  `sudo systemctl restart deckjay`. The program reads the music folders
  when it starts.
- **A new version:** download and install it as in step 3, then
  `sudo systemctl restart deckjay`.
- **Check after a change:** `deckjay check-config /etc/deckjay/config.toml`
  reads the file and can run any time. For the full check, stop the
  service first, because it holds the deck:

  ```sh
  sudo systemctl stop deckjay
  sudo -u deckjay deckjay check /etc/deckjay/config.toml
  sudo systemctl start deckjay
  ```

- **Spotify:** set up the Spotify app and `[spotify]` as in
  [spotify.md](spotify.md), start the service once (it makes
  `/var/lib/deckjay`), then sign in and restart:

  ```sh
  sudo -u deckjay deckjay spotify-login /etc/deckjay/config.toml
  sudo systemctl restart deckjay
  ```

  The Pi has no browser: open the address it prints on your computer, and
  paste the address the browser ends up at back into the terminal. Or
  connect with `ssh -L 8898:127.0.0.1:8898 you@deckjay.local` first, and
  the sign-in finishes by itself.
- **Firewall:** deckjay needs no firewall change. If you turn on `ufw`,
  allow SSH (22/tcp) and, for a Chromecast or HEOS speaker, the music
  server (8765/tcp, `http_port`): the speaker downloads the music from it.

## ⬆️ Upgrading from kids-deck

The program was called kids-deck before. An old install keeps that name
until you move it: the service, the binary, the config and state folders
and the service user. The music can stay where it is, because
`config.toml` names its folders.

A service install (steps 3 to 7), on the Pi:

```sh
sudo systemctl disable --now kids-deck
sudo rm /etc/systemd/system/kids-deck.service /usr/local/bin/kids-deck
sudo usermod --login deckjay --home /var/lib/deckjay kidsdeck
sudo groupmod --new-name deckjay kidsdeck
sudo mv /etc/kids-deck /etc/deckjay
sudo mv /var/lib/kids-deck /var/lib/deckjay
```

Then do step 3 (the new binary) and step 7 (the new unit) again. The old
unit must be gone first: two services would fight over the deck. The
hostname can stay; use your old one where this guide says
`deckjay.local`.

A Docker install, on the Pi, then from your computer:

```sh
cd ~/kids-deck && docker compose down && cd && mv kids-deck deckjay
docker image rm kids-deck:latest
```

```sh
just deploy
```

`just deploy` now uses `~/deckjay` (set `PI_DIR` in `.env` if you changed
it), and the config, music and state move with the folder.

## 🔵 Play on a Bluetooth speaker

> **Not tested yet:** a real Bluetooth speaker, and raspotify together with
> deckjay. These steps follow the Debian packages, their manuals and a
> test in a Debian container. One risk is known: after a playlist, the
> first press on a book may fail because Spotify still holds the speaker.
> The next press should play (see the end of
> [Spotify on the Pi itself](#-spotify-on-the-pi-itself)).

To deckjay a paired Bluetooth speaker is one more sound output, so it
plays with `speaker_type = "local"`. (On a Mac: pair the speaker in System
Settings, then set `audio_device` to part of its name; `deckjay check` lists the
names.)

Raspberry Pi OS Lite has no sound server, and PipeWire runs only in a
user's login session, which the `deckjay` service user never has.
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

3. Make the speaker the Pi's default sound output, for deckjay and every
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

4. Leave `audio_device` out of `/etc/deckjay/config.toml` (it plays on
   the default output), check, and restart:

   ```sh
   sudo systemctl stop deckjay
   sudo -u deckjay deckjay check /etc/deckjay/config.toml
   speaker-test -D default -c 2 -t sine -l 1   # a short tone on the speaker
   sudo systemctl start deckjay
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
list. Not tested with deckjay yet (see the note at the top of
[Play on a Bluetooth speaker](#-play-on-a-bluetooth-speaker)).

1. Install raspotify (it has arm64 packages):

   ```sh
   sudo apt install -y curl
   curl -sL https://dtcooper.github.io/raspotify/install.sh | sh
   ```

2. In `/etc/raspotify/conf`, set the name, and let it keep the Spotify
   sign-in, so the Pi stays in your account's device list after a reboot:

   ```sh
   LIBRESPOT_NAME="deckjay"
   #LIBRESPOT_DISABLE_CREDENTIAL_CACHE=
   ```

   (Put a `#` in front of the `LIBRESPOT_DISABLE_CREDENTIAL_CACHE=` line.)
   Then `sudo systemctl restart raspotify`.

3. Sign it in once: on a phone on the same network, open Spotify, tap the
   devices icon and pick **deckjay**. It keeps the sign-in in
   `/var/lib/raspotify`.

4. In `/etc/deckjay/config.toml`, set the device, and sign deckjay in
   to Spotify as in [Day to day](#-day-to-day):

   ```toml
   [spotify]
   client_id = "..."
   device = "deckjay"
   ```

   `deckjay check` lists the Spotify devices your account sees; `deckjay`
   must be one of them.

deckjay closes its sound output when a playlist starts, and pauses
Spotify when an album, a book or a station starts. librespot should free
the output when it pauses, but the pause reaches the Pi through Spotify's
servers and can take a moment: the first press after a playlist may fail,
and the next one play, because `bluez-alsa` may not let two programs use
the speaker at once.

## 🪶 Run on a Raspberry Pi Zero 2 W

> **Not tested yet:** these notes follow the specifications of the board
> and the reports of other users, not a Zero 2 W with deckjay on it.

The Zero 2 W has the CPU cores of a Pi 3 (at 1 GHz in place of 1.2 GHz)
and runs the 64-bit OS, so the steps above apply as they are: the same
arm64 binary, the same service. Half the memory and one USB port change a
few things:

- **Power.** The deck gets its 5 V straight from the Pi's micro-USB
  input: the Zero 2 W has no fuse and no current limiter on its data
  port. The budget is fine (the Pi
  [draws](https://www.cnx-software.com/2021/12/09/raspberry-pi-zero-2-w-power-consumption/)
  about 0.13 A idle with Wi-Fi and under 1.5 A even in a stress test, a
  Stream Deck MK.2 about
  [0.3 A](https://www.jamiebalfour.scot/reviews/posts/elgato-stream-deck-mk2),
  and the official supply gives 2.5 A), but a thin cable or a cheap
  adapter drops volts, and then the deck resets. Use the official
  5.1 V 2.5 A supply and a short OTG adapter that grounds the ID pin: a
  plain charging cable does not make the Pi a USB host. If the deck still
  resets, or it is an XL (it draws more), put a powered USB hub between
  the Pi and the deck.
- **One USB port.** The deck takes it. A USB sound card or a Bluetooth
  adapter needs a hub.
- **No headphone jack.** Local audio needs HDMI or a USB sound card.
  Chromecast and HEOS are not affected.
- **Wi-Fi only, 2.4 GHz.** There is no network socket, so set the Wi-Fi in
  Imager (step 1). Enough for music. The Bluetooth note above applies
  here too (same radio chip), and without a network cable the way out of
  a stutter is a USB Bluetooth adapter on a hub.
- **512 MB of memory.** Install the release binary as in step 3, and skip
  [Run it in Docker instead](#-run-it-in-docker-instead). Building on
  the Pi is out too.
- **The first draw is slower.** At each start deckjay decodes the cover
  of every item on the deck's shelves to make its tile; at 1 GHz with
  big covers this takes some seconds. After that the tiles are kept.

## 🧱 Build the binary yourself

Build it on your computer with Docker; a Pi 3 has too little memory to
build it. The image builds on Debian trixie, so this binary needs Raspberry
Pi OS trixie. In a checkout of this repository:

```sh
just image
id=$(docker create --platform linux/arm64 deckjay)
docker cp "$id":/usr/local/bin/deckjay ./deckjay
docker rm "$id"
scp deckjay config.example.toml 99-streamdeck.rules you@deckjay.local:
```

Then go on with step 3 from `sudo install`, in your home folder on the Pi.

## 🐳 Run it in Docker instead

The repository can also run deckjay on the Pi in a Docker container. Put
Docker on the Pi; then on your computer, in a checkout of this
repository:

```sh
just deploy
just pi-logs
```

`just deploy` builds the arm64 image, copies it to the Pi with
`docker save | ssh docker load`, copies `docker-compose.yml`, `config.toml`
and `music/` to `~/deckjay`, and runs `docker compose up -d` there. It
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
