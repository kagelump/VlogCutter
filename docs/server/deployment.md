# Ubuntu and Docker Compose deployment

The checked-in deployment targets Ubuntu 24.04 on an x86-64 home server. The backend uses host networking but accepts only `127.0.0.1:8090`; Caddy or Tailscale Serve supplies HTTPS. Do not add a Compose `ports` entry or bind the backend to a LAN address.

## Host preparation

Install Git, Docker Engine with the Compose plugin, and either NFS client support (preferred) or CIFS utilities (supported). Mount originals on the host before starting Compose. Example host paths are placeholders:

```bash
sudo install -d -o 10001 -g 10001 /srv/rcdvlog/derived /srv/rcdvlog/backups
sudo install -d /mnt/rcdvlog/originals
```

Configure the NAS mount in `/etc/fstab` using the NAS vendor's recommended options. For NFS, use a read-only mount such as `ro,nofail,_netdev`; for SMB, use `ro,nofail,_netdev` and a root-owned credentials file rather than embedding a password in the repository. Validate with `findmnt` and a read-only directory listing. VlogCutter never mounts shares or edits `fstab` itself.

Clone the repository, copy the example environment, and edit only existing host directories:

```bash
cp deploy/server/example.env deploy/server/.env
docker compose --env-file deploy/server/.env -f deploy/server/compose.yaml config --quiet
docker compose --env-file deploy/server/.env -f deploy/server/compose.yaml build
docker compose --env-file deploy/server/.env -f deploy/server/compose.yaml up -d
curl --fail http://127.0.0.1:8090/api/v1/health
curl --fail http://127.0.0.1:8090/api/v1/ready
```

The three bind-source directories must exist; `create_host_path: false` turns path typos into startup failures. `/media/originals` is read-only in the container. `/srv/rcdvlog/derived` and `/srv/rcdvlog/backups` are separate writable binds. `/var/lib/rcdvlog` is a Docker-local named volume and must remain on local SSD, not NFS, SMB, Syncthing, Dropbox, or another network filesystem.

The image runs as UID/GID 10001, with a read-only container filesystem, all capabilities dropped, a 64 MiB `/tmp`, bounded JSON logs, `unless-stopped` restart policy, and a 65-second Compose stop grace period. FFmpeg, ffprobe, ExifTool, and the built web application are included.

## HTTPS front doors

For LAN access, follow [security.md](security.md) and the checked-in [`Caddyfile`](../../deploy/server/Caddyfile). For private remote access, follow [`tailscale-setup.md`](../../deploy/server/tailscale-setup.md). Funnel is forbidden.

## Upgrade

Create and inspect a backup before upgrading. Then fetch the intended revision and rebuild:

```bash
docker compose --env-file deploy/server/.env -f deploy/server/compose.yaml build --pull
docker compose --env-file deploy/server/.env -f deploy/server/compose.yaml up -d
curl --fail http://127.0.0.1:8090/api/v1/ready
```

Migrations are forward-only and transactional. Do not downgrade against migrated live state and do not copy live SQLite/WAL files. Keep the prior image revision and the pre-upgrade verified restore point until Chrome, the CLI, and one adopted project are checked. A rollback that requires data rollback must use the explicit new-root restore process in [backup-and-recovery.md](backup-and-recovery.md).

## Evidence boundary

The deployment invariants are statically tested, but Docker was unavailable during Task 19; Compose rendering and the real Ubuntu/NAS topology were not executed. Complete the 17-row target-topology matrix in the [acceptance report](../tasks/server/ACCEPTANCE_TASK_19.md) before calling a release deployment accepted.
