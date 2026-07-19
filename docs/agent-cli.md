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
3. The legacy desktop catalog under macOS Application Support

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

### Projects

```bash
rcdvlog project create --name "Expo 2025"
rcdvlog project list
rcdvlog project status --project <project-id>
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
rcdvlog clips get --asset <asset-id>
rcdvlog clips update \
  --asset <asset-id> \
  --notes "Arrival establishing shot" \
  --status reviewed \
  --included true \
  --source-in 1.0 \
  --source-out 7.0
```

`clips update` is a patch operation: omitted fields retain their current values. Status values are `unreviewed`, `reviewed`, `skipped`, and `needs-attention`.

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
```

`proxy missing` may be long-running. Completed proxies remain committed if the command is interrupted.

### Thumbnails

```bash
rcdvlog thumbnails generate --asset <asset-id> --quality 1
rcdvlog thumbnails missing --project <project-id> --quality 1
```

Quality 1 is a 480-pixel-wide JPEG sampled at 20% of the clip. It is stored under the project folder at `cache/thumbnails/<asset-id>/q1.jpg`. The original is preferred; an available proxy is used when the original is offline. Qualities 2–10 are reserved and currently rejected.

### Export

```bash
rcdvlog export otio --project <project-id>
```

Export includes assets marked `included`, in best-datetime order, as one video track with hard cuts and linked source audio where Resolve supports it.

## Concurrency

SQLite runs in WAL mode. Read commands can run while ingest is writing, as verified against the Expo acceptance project. Agents should still avoid simultaneous mutations to the same asset until explicit revision tokens are added.
