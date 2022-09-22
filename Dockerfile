FROM rust:1-slim-buster as builder

WORKDIR /usr/src/splittarr
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo install --path .

FROM debian:buster-slim

ENV SPLITTARR_DATA_DIR=/config

RUN apt-get update && apt-get install -y cuetools shntool flac && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/cargo/bin/splittarr /usr/local/bin/splittarr

CMD ["splittarr"]