ARG UPSTREAM_IMAGE
ARG UPSTREAM_DIGEST_AMD64

FROM rust:1-slim-buster as builder

ARG VERSION
RUN apt-get update && apt-get install -y git pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

ENV OPENSSL_STATIC=yes
ENV OPENSSL_LIB_DIR=/usr/lib/x86_64-linux-gnu/
ENV OPENSSL_INCLUDE_DIR=/usr/include/openssl/

RUN git clone -n https://github.com/gnarr/splittarr.git /splittarr && cd /splittarr && \
    git fetch --all --tags && \
    git checkout v${VERSION} -b hotio && \
    CARGO_INSTALL_ROOT=/splittarr cargo install --locked --path .

FROM ${UPSTREAM_IMAGE}@${UPSTREAM_DIGEST_AMD64}

VOLUME ["${CONFIG_DIR}"]

COPY --from=builder /splittarr/bin/splittarr ${APP_DIR}/splittarr
COPY --from=builder /splittarr/config.toml.example ${APP_DIR}/config.toml
RUN chmod 755 "${APP_DIR}/splittarr"
COPY root /
RUN apt-get update && apt-get install -y cuetools shntool flac && rm -rf /var/lib/apt/lists/*
RUN chmod -R +x /etc/cont-init.d/ /etc/services.d/