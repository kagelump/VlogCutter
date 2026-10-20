# RCD VlogCutter agent CLI

`rcdvlog` is the non-interactive JSON control surface for desktop catalogs and the authenticated companion server. Local commands call the same Rust core as Tauri; remote commands use bounded documented HTTP operations rather than opening server databases.

## The CLI and the project-local agent workspace

This document describes the CLI as implemented today.

The accepted product architecture (PRD §6.3, §15, §25.1 item 18) pairs this CLI
with a project-local *agent workspace*. Issue #48 will make a VlogCutter video
project root self-describing to a general-purpose coding agent: a generated
`AGENTS.md` and `PROJECT.md` inside the project folder will tell an agent what
the project is and how to use this CLI, so a cold Claude Code or Codex session
opened at the project root needs no bespoke setup from the user.

The two layers have different jobs:

- **Workspace files** are the discovery, context, and bootstrap layer.
- **This CLI** is the authoritative transactional interface for reading and
  mutating canonical catalog and project state, with revision safety.

Workspace projections do not replace SQLite, which stays canonical. Directly
modifying a generated projection such as `PROJECT.md` or `edit-plan.json` is not
equivalent to changing VlogCutter state; changes flow back only through a
revision-safe CLI command or a future explicit, validated apply/import
operation. Human- and agent-authored files are different: the planned
`script.md` is a versioned input that may be edited directly.

None of the workspace materialization, script revisions, or edit-plan commands
exist yet. Everything below is implemented; anything marked planned is not.

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

Local desktop commands use `invalid_arguments`, `invalid_input`, `not_found`, `media_offline`, `media_tool_failed`, `stale_revision`, and `internal_error`. `stale_revision` is the only one whose correct handling is to retry: re-read the clip, re-apply the edit, send it again with the new revision. Server commands preserve the server's shared stable registry, including authentication, protocol, mount, job, backup, and security codes. The registry is [`core/rcdvlog-server/src/error_codes.rs`](../core/rcdvlog-server/src/error_codes.rs); clients must branch on `error.code`, not prose. Remote failures also return the server `requestId` when available.

Proxy generation captures FFmpeg stderr while draining its progress stream. A failed transcode keeps the stable `media_tool_failed` code and includes at most 4 KiB of child diagnostics in the JSON `error.message`; the source and output file names are replaced with `<source>` and `<output>`. This is part of the error envelope, so failures never prepend raw tool output to stderr.

Long ingest commands emit JSON progress objects to stderr every 25 assets and on completion:

```json
{"event":"ingest.progress","completed":25,"total":273,"path":"/Volumes/Card/clip.mp4"}
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

Known content found somewhere new is not discarded. Ingest records the discovered
location, and repairs the asset when its recorded original had gone offline — so
re-running ingest against a card that remounted elsewhere reconnects it. The
report separates `imported`, `duplicates`, and `relinked`.

### Smart ingest camera mappings

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 camera-mappings list \
  --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 camera-mappings rename \
  --project <project-id> --mapping <mapping-id> --camera "Sony A7C II"
rcdvlog --catalog /absolute/project/catalog.sqlite3 camera-mappings clear \
  --project <project-id> --mapping <mapping-id>
```

`camera-mappings list` is strictly read-only and returns the stable mapping ID,
folder hint, filename prefix, representative EXIF make/model, resolved camera,
and creation/update timestamps. The evidence is project-scoped and excludes an
ephemeral mount path, which is why a matching reformatted card can reuse it.

`rename` changes only the camera identity one stored evidence key resolves to.
`clear` forgets that key without touching clips, capture sequences, originals,
archives, or cache files; it is idempotent so retrying an already-completed clear
is safe. Both mutations require the project ID as well as the stable mapping ID,
so a mapping from another project cannot be renamed accidentally.

### Camera identity merge

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 cameras merge-preview \
  --project <project-id> --from "Sony wrong" --into "Sony A7C II"
rcdvlog --catalog /absolute/project/catalog.sqlite3 cameras merge \
  --project <project-id> --from "Sony wrong" --into "Sony A7C II"
```

`merge-preview` is strictly read-only. It confirms both cameras exist and
reports the source clip, capture-sequence, and Smart ingest mapping counts plus
the destination's existing capture-sequence count. `merge` changes camera
identity only: every source card keeps its capture-sequence ID, membership, and
clock epochs while joining the destination lane. Smart ingest mappings follow
the destination, and later replay or ingest that still names the old camera is
resolved through the durable project-local alias. Neither command changes
original, archive, or cache files.

### Relink

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 relink \
  --project <project-id> --folder "/Volumes/Shuttle/Trip Footage"
```

Finds originals a project has lost track of, after the user moved footage between
a laptop, an external drive, and a NAS. Matching is by content, never by filename
or path — two cards legitimately both contain `C0001.MP4`.

Unlike `ingest`, relink **never creates an asset**. Media in the folder belonging
to no asset in this project is counted as `unrecognized` and left alone, so
pointing at the wrong folder is a readable no-op rather than an import to undo.
Emits `relink.progress` events to stderr on the same 25-asset cadence as ingest.

`stillMissing` reports how many originals remain unreachable after the search,
and offline-volume diagnostics that the search resolved are acknowledged
automatically.

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
  --rotation-degrees 180 \
  --expected-revision 7
```

`clips update` is a patch operation: omitted fields retain their current values. The merged patch is converted to typed note/label/removal/rotation operations and committed atomically; unchanged fields emit nothing. Repeat `--add-label` or `--remove-label` to change multiple free-form labels. Label matching for removal is case-insensitive; the core trims, deduplicates, and validates labels. `--removed true` hides a clip from the default labeling queue without deleting its catalog row or source media. `clips list` excludes removed clips unless `--include-removed` is passed. `--rotation-degrees` controls the review-only orientation and accepts `0`, `90`, `180`, or `270`; it never changes the source file.

`--expected-revision` is the optimistic check, and an agent writing to a project a human may also have open should always send it. Pass the `revision` from the `clips get` (or `clips list`) the edit was decided from; the write is refused with `stale_revision` if the clip changed in between. This matters because the merged patch is a *whole* judgment: a patch built from a stale read carries the old label list and the old note, so applying it does not merely miss the other writer's change, it reverses it. The recovery is idempotent and cheap — read the clip again, re-apply the same edit to what it now says, and send it with the new revision. Omitting the flag keeps the previous last-write-wins behaviour for scripted writes with no basis to be stale against.

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

### Local transcription and cloud clip analysis

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe generate --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe missing --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe status --project <project-id>

rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze generate --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze missing --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze missing --project <project-id> --force
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze status --project <project-id>

rcdvlog --catalog /absolute/project/catalog.sqlite3 labels consolidate --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 labels vocabulary --project <project-id>
```

Transcription is offline and reads proxies only. Vision analysis reads proxy-derived frame packs and any accepted speech transcript, then calls OpenRouter using `OPENROUTER_API_KEY` or `~/.openrouter.key`. A normal `analyze missing` run is idempotent; `--force` replaces current derived observations and reports how many clips already had non-empty title or description fields before overwrite. Notes remain human-only. OCR is a reviewable suggestion and is not indexed as fact.

`analyze status` and `labels vocabulary` are strictly read-only. Analysis progress is emitted as JSON on stderr.

`transcribe missing` and `analyze missing` reports include a `cancelled` flag,
and the analysis report also includes `budgetExhausted`. Both are always `false`
from the CLI, which never stops a batch early; they exist because the desktop
shares these methods and can. A budget stop additionally appends a failure entry
naming the ceiling, while a cancellation does not — stopping on request is not a
failure. `labels consolidate` makes one text-only request and remaps only labels whose local authorship record says analysis created them; it never changes human-only labels.

Rough-cut selection and source ranges remain separate from the labeling phase.
The core and desktop now have an initial edit-plan/preview/OTIO path, but the
CLI does not yet expose edit-plan revision, validation, preview, export, or
history commands. That missing machine-readable surface is tracked by D01-T030
and by issue #48, which pairs it with the project-local agent workspace. When
those commands land they are the supported way for an agent to create or
activate an edit-plan revision; editing a projected `edit-plan.json` by hand is
not.

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
