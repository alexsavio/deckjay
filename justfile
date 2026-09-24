# >>> claude-files managed block - do not edit
# Requires just >= 1.31 (modules). Regenerate via `just claude::resync`.
set dotenv-load

mod? claude '.claude/just/common.just'
mod? rs '.claude/just/rust.just'

test:
    @just rs::test

build:
    @just rs::build

lint:
    @just rs::lint

format:
    @just rs::format

typecheck:
    @just rs::typecheck
# <<< claude-files managed block

# Project recipes. They call cargo, docker and ssh directly, so they work
# without the claude-files modules above. `[default]` needs just >= 1.43.

# pipefail: a failing `docker save` must stop `deploy`, not only `docker load`.
set shell := ["bash", "-euo", "pipefail", "-c"]

# Override in `.env` or the environment.
pi := env("PI_HOST", "pi@raspberrypi.local")
pi_dir := env("PI_DIR", "~/kids-deck")

# List all recipes
[default]
help:
    @just --list

# Create config.toml and the music folder if they don't exist
setup:
    @test -f config.toml || (cp config.example.toml config.toml && echo "created config.toml: set speaker_host")
    @mkdir -p music

# Run the player (quit the Elgato Stream Deck app first)
run *ARGS:
    cargo run --release -- {{ARGS}}

# Run with debug logs, including every file request from the speaker
debug *ARGS:
    RUST_LOG=debug,tower_http=debug cargo run -- {{ARGS}}

# List albums, Stream Decks and speaker status, then exit
doctor:
    cargo run --release -- --check

# Draw the 15-key layout into a picture, no hardware needed
preview FILE="layout.png":
    cargo run --release -- --preview {{FILE}}

# Run the tests whose name contains FILTER, e.g. `just test-match config::`
test-match FILTER:
    cargo test {{FILTER}}

# Check formatting without changing files
fmt-check:
    cargo fmt --all -- --check

# Lint with every warning as an error
clippy:
    cargo clippy --all-targets -- -D warnings

# Build the API docs, including private items (this is a binary crate)
doc *ARGS:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items {{ARGS}}

# Every check to pass before a commit: format, lint, tests, docs
ci: fmt-check clippy
    cargo test
    @just doc

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
    HOST_IP="$HOST_IP" DECK_MODEL="{{MODEL}}" docker compose -f compose.sim.yaml up --build

# Stop and remove the simulator containers
sim-down:
    docker compose -f compose.sim.yaml down

# Build the Raspberry Pi image (linux/arm64)
image:
    docker buildx build --platform linux/arm64 -t kids-deck --load .

# Copy the image, compose file, config and music to the Pi, then start it
deploy: image
    docker save kids-deck | ssh {{pi}} docker load
    ssh {{pi}} "mkdir -p {{pi_dir}}/music"
    scp docker-compose.yml config.toml {{pi}}:{{pi_dir}}/
    rsync -a music/ {{pi}}:{{pi_dir}}/music/
    ssh {{pi}} "cd {{pi_dir}} && docker compose up -d"

# Make the Pi's music folder match ./music exactly: deletes albums not here
[confirm("Delete music on the Pi that is not in ./music? [y/N]")]
pi-music-prune:
    rsync -a --delete music/ {{pi}}:{{pi_dir}}/music/

# Follow the logs on the Pi
pi-logs:
    ssh -t {{pi}} "cd {{pi_dir}} && docker compose logs -f"
