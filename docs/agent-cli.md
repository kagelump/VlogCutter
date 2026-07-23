# RCD VlogCutter agent CLI

`rcdvlog` is the non-interactive control surface for Codex, Claude, and shell automation. It calls the same Rust core and SQLite catalog as the Tauri desktop app.

## Build and catalog selection

```bash
cargo build -p rcdvlog-cli
target/debug/rcdvlog doctor
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

Current error codes are `invalid_arguments`, `invalid_input`, `not_found`, `media_offline`, `media_tool_failed`, and `internal_error`.

Long ingest commands emit JSON progress objects to stderr every 25 assets and on completion:

```json
{"event":"ingest.progress","completed":25,"total":273,"path":"/Volumes/data/clip.mp4"}
```

## Commands

### Prerequisites

```bash
rcdvlog doctor
```

### Screenshot-driven diagnosis

The desktop persists the last displayed catalog, project, and clip whenever selection changes. This is a diagnostic breadcrumb, not a live UI-control channel. Start screenshot investigations with:

```bash
rcdvlog desktop last-opened
rcdvlog catalog inspect
rcdvlog project diagnose --project <project-id>
rcdvlog clips diagnose --asset <asset-id>
```

`catalog inspect`, `project diagnose`, `clips diagnose`, and `proxy status` open SQLite strictly read-only and never run migrations. Catalog inspection includes unresolved operation-conflict count when schema version 5 is present. Project diagnosis includes operation/conflict state, and clip diagnosis includes materialized revision plus immutable note history alongside the stored asset row, live ffprobe evidence, cache-version classification, computed render transform/final dimensions, warnings, and recommended explicit actions. It is the preferred evidence bundle when a screenshot disagrees with catalog labels.

The pattern is intentionally extensible: persist a stable ID for newly selectable UI entities, add a focused read-only `diagnose` projection in the Rust core/CLI, report stored state separately from observed external state, explain the derived behavior, and expose mutations as separate commands.

### Catalog inspection

```bash
rcdvlog catalog inspect
```

Reports required tables, pending diagnostic/schema columns, journal mode, project/asset counts, and unresolved operation conflicts without changing the file.

### Projects

```bash
rcdvlog project create --name "Expo 2025"
rcdvlog project list
rcdvlog project status --project <project-id>
rcdvlog project diagnose --project <project-id>
```

### Ingest

```bash
rcdvlog ingest --project <project-id> --folder "/Volumes/data/VideoProjects/2025-08 Expo"
```

Ingest references media in place and is idempotent within a project by sampled fingerprint.

### Clips

```bash
rcdvlog clips list --project <project-id> --limit 100 --offset 0
rcdvlog clips list --project <project-id> --query "arrival"
rcdvlog clips list --project <project-id> --include-removed
rcdvlog clips get --asset <asset-id>
rcdvlog clips diagnose --asset <asset-id>
rcdvlog clips refresh-orientation --asset <asset-id>
rcdvlog clips update \
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
rcdvlog clock apply \
  --project <project-id> \
  --camera "DJI Pocket 3" \
  --start-filename DJI_20250810104709_0002_D.MP4 \
  --offset-minutes -60 \
  --label "Camera timezone changed"
```

The fixed offset applies from the filename boundary onward for that camera. It does not alter source metadata.

### Proxies

```bash
rcdvlog proxy generate --asset <asset-id>
rcdvlog proxy missing --project <project-id>
rcdvlog proxy status --asset <asset-id>
rcdvlog proxy status --project <project-id> --limit 100
```

`proxy status` is read-only and distinguishes verified raw-orientation v3 proxies, obsolete metadata-tagged raw v2 proxies, legacy metadata-baked v1 proxies, and missing files. Listed v3 items are live-probed and are ready only when their display rotation is 0°. `proxy missing` may be long-running and regenerates obsolete representations when originals are available. Completed proxies remain committed if the command is interrupted.

### Thumbnails

```bash
rcdvlog thumbnails generate --asset <asset-id> --quality 1
rcdvlog thumbnails missing --project <project-id> --quality 1
```

Quality 1 is a 480-pixel-wide JPEG sampled at 20% of the clip. It is stored under the project folder at `cache/thumbnails/<asset-id>/q1.v2.jpg`. The original is preferred; an available proxy is used when the original is offline. Qualities 2–10 are reserved and currently rejected.

Rough-cut selection, source ranges, and OTIO export are intentionally absent from this labeling phase. They will return as a separate edit-planning workflow after labeling.

## Concurrency

SQLite runs in WAL mode. Read commands can run while ingest is writing, as verified against the Expo acceptance project. Agents should still avoid simultaneous mutations to the same asset until explicit revision tokens are added.
