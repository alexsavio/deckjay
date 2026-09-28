# Builds the Raspberry Pi image (needs 64-bit Raspberry Pi OS on the Pi).
#
#   docker buildx build --platform linux/arm64 -t deckjay --load .
#
# On an Apple Silicon Mac this builds natively and quickly. On an Intel Mac it
# runs under emulation and takes a while.

# Keep the Rust version in step with rust-toolchain.toml.
FROM rust:1.98-trixie AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends libudev-dev libasound2-dev pkg-config \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src

# Build the dependencies first so code changes don't rebuild them.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
 && cargo build --release --locked \
 && rm -rf src

COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends libudev1 libasound2t64 ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/deckjay /usr/local/bin/deckjay
WORKDIR /app
ENTRYPOINT ["deckjay"]
CMD ["/app/config.toml"]
