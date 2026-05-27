# syntax=docker/dockerfile:1

FROM rust:1.95-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/app/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    cargo build --release && \
    mkdir -p /app/build && \
    cp /app/target/release/splittarr /app/build/splittarr

FROM debian:bookworm-slim

ENV SPLITTARR_DATA_DIR=/config

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates cuetools flac shntool && \
    rm -rf /var/lib/apt/lists/* && \
    mkdir -p /config /data

WORKDIR /app
COPY --from=builder /app/build/splittarr /usr/local/bin/splittarr

CMD ["splittarr"]
