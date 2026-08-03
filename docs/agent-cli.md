# RCD VlogCutter agent CLI

`rcdvlog` is the non-interactive JSON control surface for desktop catalogs and the authenticated companion server. Local commands call the same Rust core as Tauri; remote commands use bounded documented HTTP operations rather than opening server databases.

## Build and catalog selection

```bash
cargo build -p rcdvlog-cli
target/debug/rcdvlog --catalog /absolute/project/catalog.sqlite3 doctor
```

Catalog selection, in precedence order:

1. `--catalog /absolute/path/catalog.sqlite3`
2. `RCDVLOG_CATALOG`
3. The desktop app's persisted `lastCatalogPath`
4. The legacy desktop catalog under macOS Application Support

Agents should pass `--catalog <project folder>/catalog.sqlite3` explicitly for desktop projects, tests, and temporary workflows.

## Output contract

Success is one JSON value on stdout with exit code `0`:

```json
{
  "ok": true,
  "command": "project.list",
  "data": []
}
```

Failure is one JSON value on stderr with a nonzero exit code:

```json
{
  "ok": false,
  "command": "clips.get",
  "error": {
    "code": "not_found",
    "message": "clips.get failed: asset not found: asset_missing"
  }
}
```

Local desktop commands use `invalid_arguments`, `invalid_input`, `not_found`, `media_offline`, `media_tool_failed`, and `internal_error`. Server commands preserve the server's shared stable registry, including authentication, protocol, mount, job, backup, and security codes. The registry is [`core/rcdvlog-server/src/error_codes.rs`](../core/rcdvlog-server/src/error_codes.rs); clients must branch on `error.code`, not prose. Remote failures also return the server `requestId` when available.

Long ingest commands emit JSON progress objects to stderr every 25 assets and on completion:

```json
{"event":"ingest.progress","completed":25,"total":273,"path":"/Volumes/data/clip.mp4"}
```

## Commands

### Prerequisites

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 doctor
```

### Screenshot-driven diagnosis

The desktop persists the last displayed catalog, project, and clip whenever selection changes. This is a diagnostic breadcrumb, not a live UI-control channel. Start screenshot investigations with:

```bash
rcdvlog desktop last-opened
rcdvlog --catalog /absolute/project/catalog.sqlite3 catalog inspect
rcdvlog --catalog /absolute/project/catalog.sqlite3 project diagnose --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips diagnose --asset <asset-id>
```

`catalog inspect`, `project diagnose`, `clips diagnose`, and `proxy status` open SQLite strictly read-only and never run migrations. Catalog inspection includes unresolved operation-conflict counts for operation-aware catalogs. Project diagnosis includes operation/conflict state, and clip diagnosis includes materialized revision plus immutable note history alongside the stored asset row, live ffprobe evidence, cache-version classification, computed render transform/final dimensions, warnings, and recommended explicit actions. It is the preferred evidence bundle when a screenshot disagrees with catalog labels.

The pattern is intentionally extensible: persist a stable ID for newly selectable UI entities, add a focused read-only `diagnose` projection in the Rust core/CLI, report stored state separately from observed external state, explain the derived behavior, and expose mutations as separate commands.

### Catalog inspection

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 catalog inspect
```

Reports required tables, pending diagnostic/schema columns, journal mode, project/asset counts, and unresolved operation conflicts without changing the file.

### Projects

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 project create --name "Expo 2025"
rcdvlog --catalog /absolute/project/catalog.sqlite3 project list
rcdvlog --catalog /absolute/project/catalog.sqlite3 project status --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 project diagnose --project <project-id>
```

### Ingest

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 ingest \
  --project <project-id> --folder "/absolute/source/Trip Footage"
```

Ingest references media in place and is idempotent within a project by sampled fingerprint.

### Clips

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips list --project <project-id> --limit 100 --offset 0
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips list --project <project-id> --query "arrival"
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips list --project <project-id> --include-removed
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips get --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips diagnose --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips refresh-orientation --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips update \
  --asset <asset-id> \
  --notes "Arrival establishing shot" \
  --add-label arrival \
  --add-label establishing \
  --remove-label unusable \
  --removed false \
  --rotation-degrees 180
```

`clips update` is a patch operation: omitted fields retain their current values. The merged patch is converted to typed note/label/removal/rotation operations and committed atomically; unchanged fields emit nothing. Repeat `--add-label` or `--remove-label` to change multiple free-form labels. Label matching for removal is case-insensitive; the core trims, deduplicates, and validates labels. `--removed true` hides a clip from the default labeling queue without deleting its catalog row or source media. `clips list` excludes removed clips unless `--include-removed` is passed. `--rotation-degrees` controls the review-only orientation and accepts `0`, `90`, `180`, or `270`; it never changes the source file.

`clips refresh-orientation` is the explicit mutating counterpart to diagnosis. It probes the immutable original, marks orientation as known even when the answer is 0°, preserves a user override, and updates only catalog metadata.

### Camera clock events

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 clock apply \
  --project <project-id> \
  --camera "DJI Pocket 3" \
  --start-filename DJI_20250810104709_0002_D.MP4 \
  --offset-minutes -60 \
  --label "Camera timezone changed"
```

The fixed offset applies from the filename boundary onward for that camera. It does not alter source metadata.

### Proxies

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 proxy generate --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 proxy missing --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 proxy status --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 proxy status --project <project-id> --limit 100
```

`proxy status` is read-only and distinguishes verified raw-orientation v3 proxies, obsolete metadata-tagged raw v2 proxies, legacy metadata-baked v1 proxies, and missing files. Listed v3 items are live-probed and are ready only when their display rotation is 0°. `proxy missing` may be long-running and regenerates obsolete representations when originals are available. Completed proxies remain committed if the command is interrupted.

### Thumbnails

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 thumbnails generate --asset <asset-id> --quality 1
rcdvlog --catalog /absolute/project/catalog.sqlite3 thumbnails missing --project <project-id> --quality 1
```

Quality 1 is a 480-pixel-wide JPEG sampled at 20% of the clip. It is stored under the project folder at `cache/thumbnails/<asset-id>/q1.v2.jpg`. The original is preferred; an available proxy is used when the original is offline. Qualities 2–10 are reserved and currently rejected.

Rough-cut selection and source ranges remain separate from the labeling phase.
The core and desktop now have an initial edit-plan/preview/OTIO path, but the
CLI does not yet expose edit-plan revision, validation, preview, export, or
history commands. That missing machine-readable surface is tracked by D01-T030.

## Companion-server commands

Every remote command requires an explicit endpoint, either `--endpoint` or `RCDVLOG_SERVER_ENDPOINT`:

```bash
rcdvlog server --endpoint https://<server-host>:8443 status
```

Credential precedence is:

1. `RCDVLOG_SERVER_TOKEN`.
2. `--credential-file` or `RCDVLOG_SERVER_CREDENTIAL_FILE`; the file must be owner-only.
3. Interactive macOS Keychain lookup for the returned stable server node ID.

Prefer an owner-only credential file for unattended automation. Never put the token in argv, a URL, output, a fixture, or a repository file. Every success includes request endpoint, non-secret credential-source label, protocol/version identity, and result.

All list operations are bounded and cursor-based where the API supports pagination:

```bash
rcdvlog server --endpoint https://<server-host>:8443 projects --limit 50
rcdvlog server --endpoint https://<server-host>:8443 library list --limit 50 --filename-prefix DJI --camera "Pocket 3"
rcdvlog server --endpoint https://<server-host>:8443 library diagnose --media-object <media-object-id>
rcdvlog server --endpoint https://<server-host>:8443 storage list
rcdvlog server --endpoint https://<server-host>:8443 storage diagnose --root <root-id>
rcdvlog server --endpoint https://<server-host>:8443 scans list --limit 50
rcdvlog server --endpoint https://<server-host>:8443 scans diagnose --scan <scan-id>
rcdvlog server --endpoint https://<server-host>:8443 jobs list --limit 50 --state failed
rcdvlog server --endpoint https://<server-host>:8443 jobs diagnose --job <job-id>
rcdvlog server --endpoint https://<server-host>:8443 sync status
rcdvlog server --endpoint https://<server-host>:8443 sync diagnose --project <project-id>
rcdvlog server --endpoint https://<server-host>:8443 representations status --representation <representation-id>
rcdvlog server --endpoint https://<server-host>:8443 credentials list
rcdvlog server --endpoint https://<server-host>:8443 backups list
rcdvlog server --endpoint https://<server-host>:8443 backups diagnose --backup <backup-id>
```

Read commands may inspect already-known filesystem/network/tool state but never migrate, scan broadly, hash a full file, enqueue, repair, change cursors, or create cache output. Their JSON separates stored state, live observation, derived decisions, version identity, warnings, and recommended explicit mutations.

Mutations are deliberately named:

```bash
rcdvlog server --endpoint https://<server-host>:8443 scans request \
  --root <root-id> --scope <relative/subtree> --idempotency-key <unique-key>
rcdvlog server --endpoint https://<server-host>:8443 jobs retry --job <job-id>
rcdvlog server --endpoint https://<server-host>:8443 jobs cancel --job <job-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 server \
  --endpoint https://<server-host>:8443 sync now --project <project-id>
rcdvlog server --endpoint https://<server-host>:8443 representations request \
  --media-object <media-object-id> --kind proxy
rcdvlog server --endpoint https://<server-host>:8443 credentials pairing-create \
  --output-file /absolute/new/pairing-code.txt
rcdvlog server --endpoint https://<server-host>:8443 credentials revoke --credential <credential-id>
rcdvlog server --endpoint https://<server-host>:8443 backups create --idempotency-key <unique-key>
```

Pairing-code creation removes the plaintext code from JSON and writes it once to a new owner-only file. `sync now` signals the running desktop when it owns the project sync manager; otherwise it performs one exclusive bounded reconciliation and reports which path ran.

## Server-local administration

`rcdvlog-server admin` is the offline, server-host-only recovery surface. It uses environment-selected data/backup roots, returns one JSON result, and refuses exclusive work against a running server.

```bash
rcdvlog-server admin bootstrap create --output-file /absolute/new/bootstrap-code.txt
rcdvlog-server admin backup inspect --backup /absolute/backup/restore-point
rcdvlog-server admin restore inspect --restore-point /absolute/backup/restore-point
rcdvlog-server admin restore apply \
  --restore-point /absolute/backup/restore-point \
  --new-data-root /absolute/new-server-data \
  --confirm-new-data-root
rcdvlog-server admin diagnostics bundle --output-directory /absolute/new/diagnostic-directory
```

Restore always targets a new directory and requires the server to be stopped. Sequence-divergence acceptance is intentionally omitted from routine examples; use it only after following the investigation in [backup-and-recovery.md](server/backup-and-recovery.md).

## Concurrency

Desktop and server SQLite files use WAL mode only for same-host concurrency. Read commands can run while another local connection holds an uncommitted write. Cross-node concurrency uses immutable typed operations, per-origin cursors, deterministic materialization, and explicit conflicts—not shared SQLite files. Avoid competing full-patch desktop/CLI edits to the same asset fields; use the operation-specific server/web mutations where available.
