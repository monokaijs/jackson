# syntax=docker/dockerfile:1.7
FROM rust:1.93-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
ARG JACKSON_RELEASE_VERSION
RUN JACKSON_RELEASE_VERSION="${JACKSON_RELEASE_VERSION}" cargo build --locked --release

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates python3 python3-pip \
    && python3 -m pip install --no-cache-dir --break-system-packages yt-dlp \
    && apt-get purge -y python3-pip \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 jackson \
    && install -d -o jackson -g jackson /app/data
WORKDIR /app
COPY --from=builder /build/target/release/jackson /usr/local/bin/jackson
USER jackson

ENV DATABASE_URL=sqlite://data/jackson.db?mode=rwc \
    IDLE_DISCONNECT_SECS=300 \
    MAX_PLAYLIST_TRACKS=100 \
    RUST_LOG=jackson=info,songbird=warn,serenity=warn
VOLUME ["/app/data"]
HEALTHCHECK --interval=30s --timeout=3s --retries=3 CMD ["/bin/sh", "-c", "kill -0 1"]
ENTRYPOINT ["jackson"]
