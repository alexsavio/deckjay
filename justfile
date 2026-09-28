# >>> claude-files managed block - do not edit
# Requires just >= 1.31 (modules). Regenerate via `just claude::resync`.
set dotenv-load

mod? claude '.claude/just/common.just'
mod? rs '.claude/just/rust.just'
# <<< claude-files managed block

# Project recipes. They call cargo, docker and ssh directly, so they work
# without the claude-files modules above. `[default]` needs just >= 1.43.

# pipefail: a failing `docker save` must stop `deploy`, not only `docker load`.
set shell := ["bash", "-euo", "pipefail", "-c"]

# Override in `.env` or the environment.
pi := env("PI_HOST", "pi@raspberrypi.local")
pi_dir := env("PI_DIR", "~/deckjay")

# List all recipes
[default]
help:
    @just --list

# Create config.toml, the music folder and the state folder if they don't exist
setup:
    @test -f config.toml || (cp config.example.toml config.toml && echo "created config.toml: set speaker_host")
    @mkdir -p music state

# Run the player (quit the Elgato Stream Deck app first)
run *ARGS:
    cargo run --release -- {{ ARGS }}

# Run with debug logs, including every file request from the speaker
debug *ARGS:
    RUST_LOG=debug,tower_http=debug cargo run -- {{ ARGS }}

# List sources and their items, Stream Decks and speaker status, then exit (2 = warnings only, e.g. no deck)
doctor:
    cargo run --release -- check

# Sign in to Spotify once (needs [spotify] in config.toml)
spotify-login:
    cargo run --release -- spotify-login

# Draw the 15-key layout into a picture, no hardware needed
preview FILE="layout.png":
    cargo run --release -- preview {{ FILE }}

# Build the debug binary
build:
    cargo build

# Install deckjay into ~/.cargo/bin
install:
    cargo install --path . --locked

# Run every test
test:
    cargo test

# Run the tests whose name contains FILTER, e.g. `just test-match config::`
test-match FILTER:
    cargo test {{ FILTER }}

# Format the code
format:
    cargo fmt --all

# Check formatting without changing files
fmt-check:
    cargo fmt --all -- --check

# Lint with every warning as an error
lint:
    cargo clippy --all-targets -- -D warnings

# Type-check every target without building
typecheck:
    cargo check --all-targets

# Build the API docs, including private items
doc *ARGS:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items {{ ARGS }}

# Every check to pass before a commit: format, lint, tests, docs
ci: fmt-check lint
    cargo test
    @just doc

# Check the dependencies for security advisories (needs cargo-audit)
audit:
    cargo audit

# Run the player and a web Stream Deck simulator in Docker (models: mk2 mini neo xl plus)
sim MODEL="mk2":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -z "${HOST_IP:-}" ]; then
        # The speaker needs this computer's LAN address, not the container's.
        iface=$(route -n get default 2>/dev/null | awk '/interface:/ {print $2}' || true)
        HOST_IP=$( { [ -n "$iface" ] && ipconfig getifaddr "$iface"; } || hostname -I | awk '{print $1}')
    fi
    echo "Simulator: http://localhost:8090   Music for the speaker: http://$HOST_IP:8765"
    files=(-f compose.sim.yaml)
    # Volumes for sources outside ./music; see compose.sim.yaml.
    if [ -f compose.sim.local.yaml ]; then files+=(-f compose.sim.local.yaml); fi
    HOST_IP="$HOST_IP" DECK_MODEL="{{ MODEL }}" docker compose "${files[@]}" up --build

# Stop and remove the simulator containers
sim-down:
    docker compose -f compose.sim.yaml down

# Build the Raspberry Pi image (linux/arm64)
image:
    docker buildx build --platform linux/arm64 -t deckjay --load .

# Copy the image, compose file, config and music to the Pi, then start it
deploy: image
    docker save deckjay | ssh {{ pi }} docker load
    ssh {{ pi }} "mkdir -p {{ pi_dir }}/music {{ pi_dir }}/state"
    scp docker-compose.yml config.toml {{ pi }}:{{ pi_dir }}/
    rsync -a music/ {{ pi }}:{{ pi_dir }}/music/
    ssh {{ pi }} "cd {{ pi_dir }} && docker compose up -d"

# Make the Pi's music folder match ./music exactly: deletes albums not here
[confirm("Delete music on the Pi that is not in ./music? [y/N]")]
pi-music-prune:
    rsync -a --delete music/ {{ pi }}:{{ pi_dir }}/music/

# Sign in to Spotify on the Pi: open the printed address here, paste back where the browser ends
pi-spotify-login:
    ssh -t {{ pi }} "cd {{ pi_dir }} && docker compose run --rm deckjay spotify-login /app/config.toml"

# Follow the logs on the Pi
pi-logs:
    ssh -t {{ pi }} "cd {{ pi_dir }} && docker compose logs -f"

# Versions are CalVer (calver.org) YYYY.MM.MICRO, e.g. 2026.9.0, tagged v2026.9.0.
# A pushed tag starts the Release workflow: binaries, GitHub release, crates.io.

# Show the current version
version:
    @sed -n '/^\[package\]/,/^\[/{s/^version = "\(.*\)"/\1/p;}' Cargo.toml

# Write CHANGELOG.md from the commit messages
changelog:
    git-cliff -o CHANGELOG.md
    rumdl fmt CHANGELOG.md

# Show the changes since the last release
changelog-preview:
    git-cliff --unreleased --strip header

# Check that the crate packages and builds as crates.io will build it
publish-dry:
    cargo publish --dry-run --locked

# Print the next CalVer version: this month's next MICRO, else YYYY.MM.0
_next-version:
    #!/usr/bin/env bash
    set -euo pipefail
    prefix="$(date +%Y).$(date +%-m)"
    current=$(just version)
    if [[ "$current" == "$prefix".* ]]; then
        echo "$prefix.$(( ${current##*.} + 1 ))"
    else
        echo "$prefix.0"
    fi

# Release VERSION: set it, run the checks, write the changelog, commit, tag, push
release VERSION:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! "{{ VERSION }}" =~ ^[0-9]{4}\.[1-9][0-9]?\.[0-9]+$ ]]; then
        echo "VERSION must be YYYY.MM.MICRO, e.g. 2026.9.0 (no leading v)" >&2
        exit 1
    fi
    if git rev-parse -q --verify "refs/tags/v{{ VERSION }}" >/dev/null; then
        echo "tag v{{ VERSION }} exists already" >&2
        exit 1
    fi
    if [ "$(git branch --show-current)" != main ]; then
        echo "release from main" >&2
        exit 1
    fi
    if ! git diff --quiet HEAD; then
        echo "commit or stash your changes first" >&2
        exit 1
    fi
    git fetch -q origin main
    if [ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]; then
        echo "main is not the same as origin/main: pull or push first" >&2
        exit 1
    fi
    # A failed step before the commit puts the files back, so a rerun gets the same version.
    trap 'git checkout -q -- Cargo.toml Cargo.lock CHANGELOG.md; rm -f Cargo.toml.bak' ERR
    sed -i.bak '/^\[package\]/,/^\[/{s/^version = ".*"/version = "{{ VERSION }}"/;}' Cargo.toml
    rm Cargo.toml.bak
    cargo check --quiet
    just ci
    git-cliff --tag "v{{ VERSION }}" -o CHANGELOG.md
    rumdl fmt CHANGELOG.md
    git add Cargo.toml Cargo.lock CHANGELOG.md
    # cliff.toml leaves this subject out of the changelog.
    git commit -m "chore(release): prepare for v{{ VERSION }}"
    git tag "v{{ VERSION }}"
    # One atomic push: the Changelog workflow must see the tag with the commit.
    git push --atomic origin main "v{{ VERSION }}"
    echo "Released v{{ VERSION }}: the Release workflow builds and publishes it."

# Release the next CalVer version
release-next:
    just release "$(just _next-version)"
