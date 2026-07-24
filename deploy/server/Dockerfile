FROM node:24-bookworm AS web-builder

WORKDIR /build
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY apps/web/package.json apps/web/package.json
COPY packages/project-client/package.json packages/project-client/package.json
COPY packages/review-ui/package.json packages/review-ui/package.json
RUN pnpm install --frozen-lockfile
COPY apps/web apps/web
COPY packages/project-client packages/project-client
COPY packages/review-ui packages/review-ui
RUN pnpm --dir apps/web build

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
COPY --from=web-builder /build/apps/web/dist /opt/rcdvlog/web

USER rcdvlog:rcdvlog

ENV RCDVLOG_DATA_DIR=/var/lib/rcdvlog \
    RCDVLOG_TEMP_DIR=/var/lib/rcdvlog/tmp \
    RCDVLOG_DERIVED_DIR=/srv/rcdvlog/derived \
    RCDVLOG_BACKUP_DIR=/srv/rcdvlog/backups \
    RCDVLOG_ALLOWED_ORIGINAL_MOUNTS=/media/originals \
    RCDVLOG_LISTEN_ADDR=127.0.0.1:8090 \
    RCDVLOG_SHUTDOWN_TIMEOUT_SECONDS=10 \
    RCDVLOG_WEB_DIR=/opt/rcdvlog/web

EXPOSE 8090
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/rcdvlog-server"]
