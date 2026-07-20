FROM rust:1.97-bookworm AS builder

WORKDIR /build
COPY . .
RUN cargo build --locked --release -p rcdvlog-server

FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        ffmpeg \
        libimage-exiftool-perl \
        tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 rcdvlog \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin rcdvlog \
    && install -d -o rcdvlog -g rcdvlog \
        /var/lib/rcdvlog \
        /var/lib/rcdvlog/tmp \
        /srv/rcdvlog/derived \
        /srv/rcdvlog/backups \
        /media/originals

COPY --from=builder /build/target/release/rcdvlog-server /usr/local/bin/rcdvlog-server

USER rcdvlog:rcdvlog

ENV RCDVLOG_DATA_DIR=/var/lib/rcdvlog \
    RCDVLOG_TEMP_DIR=/var/lib/rcdvlog/tmp \
    RCDVLOG_DERIVED_DIR=/srv/rcdvlog/derived \
    RCDVLOG_BACKUP_DIR=/srv/rcdvlog/backups \
    RCDVLOG_ALLOWED_ORIGINAL_MOUNTS=/media/originals \
    RCDVLOG_LISTEN_ADDR=127.0.0.1:8090 \
    RCDVLOG_SHUTDOWN_TIMEOUT_SECONDS=10

EXPOSE 8090
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/rcdvlog-server"]
