FROM rust:1-slim-buster as builder

WORKDIR /usr/src/splittarr

COPY . .

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

RUN cargo install --path .

FROM debian:buster-slim

RUN apt-get update && apt-get install -y cuetools shntool flac && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/cargo/bin/splittarr /usr/local/bin/splittarr

CMD ["splittarr"]