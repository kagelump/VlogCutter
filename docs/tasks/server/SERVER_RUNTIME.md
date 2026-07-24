# Companion Server Runtime Foundation

This is the implementation handoff for Task 2. It describes the private runtime contract that later server tasks extend. Repository-wide product and installation documentation remains owned by Task 20.

## Crates and ownership

- `core/rcdvlog-server` owns server configuration, the control catalog, the stable server node identity, the single-process lock, and readiness evaluation.
- `apps/server` owns only process signals, structured logging, the loopback HTTP adapter, and graceful serving.
- Later tasks add control-catalog migrations to `CONTROL_CATALOG_MIGRATIONS`; they do not edit migration 1 or put server-only tables in a portable project catalog.

The control schema is currently version `3`. Task 3's storage/library model is documented in [STORAGE_LIBRARY.md](STORAGE_LIBRARY.md). Task 4 adds a durable local server job engine; it is deliberately not a Mac queue or distributed competitive worker system.

| ID | Name | Immutable source identity | Purpose |
| --- | --- | --- | --- |
| 1 | `control_catalog_foundation` | `server-control/0001-control-catalog-foundation/20260720` | Adds the singleton server identity, redacted configuration revisions, project-registry shell, and operational metadata. |
| 2 | `storage_global_library` | `server-control/0002-storage-global-library/20260720` | Adds logical roots and bindings, global media/storage identity, membership projection, and scoped observations. |
| 3 | `durable_server_job_engine` | `server-control/0003-durable-server-job-engine/20260720` | Adds durable local job state, attempts, leases, dependencies, rate history, and replayable events. |

Every migration uses the shared numbered, contiguous, individually transactional migration API introduced by Task 1. Live state is stored at `<dataDir>/control.sqlite3` in WAL mode. `<dataDir>/server.lock` is an advisory process lock and is held for the process lifetime.

## Internal listener contract

The binary accepts only an IP loopback socket address. Its default is `127.0.0.1:8090`. Task 2 intentionally exposes no authentication, product data, scanning, jobs, storage mutation, project sync, or web application.

| Route | Behavior |
| --- | --- |
| `GET /api/v1` | Identifies the private foundation and points at the version route. |
| `GET /` | Serves the built Chrome web application when `RCDVLOG_WEB_DIR` is configured. |
| `GET /api/openapi.json` | Returns the Rust-owned OpenAPI contract used to verify the TypeScript client. |
| `GET /api/v1/health` | Non-mutating liveness response. It does not inspect or migrate SQLite. |
| `GET /api/v1/ready` | `200` when the catalog, configured directories, resolved path separation, and tools passed checks; otherwise `503` with stable check codes. |
| `GET /api/v1/version` | Reports application, protocol, control-schema, and stable server-node versions/identity. |

Every response includes a JSON `requestId` and `X-Request-ID`. Incoming IDs are honored only when they are 1–64 bytes and contain ASCII letters, digits, `.`, `-`, `_`, or `:`. Other values are replaced. HTTP completion logs carry the same ID.

`ffmpeg`, `ffprobe`, and `exiftool` absence makes readiness false but does not prevent the private listener or liveness endpoint from starting. A missing, inaccessible, or unresolvable derived, backup, or declared original directory behaves the same way. Startup creates and resolves only `dataDir` and its contained `tempDir`; it does not touch or resolve external paths. External canonicalization, symlink-aware separation, effective access, SQLite usability, and tool checks run only as readiness work.

HTTP readiness runs on Tokio's blocking pool behind a one-per-process single-flight permit. A response is bounded to two seconds. A timed-out evaluation returns `readiness_check_timeout` and retains the permit until its blocking operation actually finishes; concurrent requests return `readiness_check_busy` instead of creating more blocking work. A panic or blocking-task failure returns `readiness_check_failed`. These codes and check names do not expose configured paths. Liveness performs none of this work.

## Configuration

Configuration is environment-based and validated before binding:

| Variable | Default |
| --- | --- |
| `RCDVLOG_DATA_DIR` | `/var/lib/rcdvlog` |
| `RCDVLOG_TEMP_DIR` | `<dataDir>/tmp` |
| `RCDVLOG_DERIVED_DIR` | `<dataDir>/derived` |
| `RCDVLOG_BACKUP_DIR` | `<dataDir>/backups` |
| `RCDVLOG_ALLOWED_ORIGINAL_MOUNTS` | Empty comma-separated list |
| `RCDVLOG_LISTEN_ADDR` | `127.0.0.1:8090` |
| `RCDVLOG_SHUTDOWN_TIMEOUT_SECONDS` | `10` (allowed range 1–60) |
| `RCDVLOG_FFMPEG` | `ffmpeg` |
| `RCDVLOG_FFPROBE` | `ffprobe` |
| `RCDVLOG_EXIFTOOL` | `exiftool` |

Paths must be absolute and traversal-free. `tempDir` must resolve inside `dataDir`. Lexical validation rejects derived, backup, and temporary directory ancestry overlap while allowing separate `dataDir` children. Readiness separately rejects overlap revealed by successfully resolved symlinks and treats non-`NotFound` resolution failures as `path_resolution_failed`; a genuinely missing path remains `directory_unavailable`. Readiness uses a non-mutating effective-user access check for external paths: read/search access for originals and read/write/search access for derived and backup. Configuration errors use `invalid_configuration` and a non-secret field name. Future secrets must use `SecretString`, whose debug and serialized forms are always `[REDACTED]`.

## Compose safety model

`deploy/server/compose.yaml` is Linux-specific because it uses host networking. This is deliberate: a process bound to container loopback cannot be reached through a normal Docker port mapping, whereas host networking makes `127.0.0.1:8090` the Ubuntu host loopback without making it a LAN listener. There is no `ports` entry.

The control state uses a plain Docker-local named volume, not a NAS bind mount. On Linux, startup also rejects data directories whose reported filesystem type is NFS or SMB/CIFS. There is no portable or exhaustive way to prove that every local-looking filesystem, volume plugin, or FUSE mount is physically backed by the SSD, so the operator remains responsible for keeping Docker's volume root local; the runtime check is defense in depth, not proof of SSD media. The three configured host mount paths must already exist because bind mounts set `create_host_path: false`. The original mount is additionally declared `read_only: true`; derived and backup mounts are separate writable paths. The container root filesystem is read-only and all Linux capabilities are dropped. Task 9 will add the authenticated HTTPS front doors.

## Shutdown contract

SIGTERM and Ctrl-C cancel a shared `ShutdownCoordinator`. Axum stops accepting new connections and drains active requests. The process enforces the configured 1–60 second bound, drops the listener if that bound expires, performs a passive control-catalog WAL checkpoint, and releases the instance lock on exit. Tokio runtime shutdown does not wait again for an already timed-out blocking readiness call, so an offline filesystem cannot extend process termination indefinitely. Future workers must receive a child token from the same coordinator.
