# RCD VlogCutter

## MVP Overall Design Sketch

**Version:** 0.7
**Status:** Desktop MVP implementation and optional companion-server epic implemented; remaining product gaps and external acceptance are explicit below

---

## Current implementation checkpoint — 2026-07-28

The macOS desktop implements the local-first labeling vertical slice plus the
domain foundation for chronology and edit handoff: project catalogs,
reference-in-place ingest, metadata, notes/labels/reversible removal,
thumbnails/proxies, operation-backed chronology facts and deterministic solving,
durable diagnostics, versioned edit plans, preview generation, deterministic
OTIO writing, and JSON chronology diagnostics. The expanded calibration and
edit-plan desktop workflows remain partial, and there is still no ingest
preview, complete capture-sequence editor, edit-plan CLI, project-local agent
workspace, Resolve import acceptance, relink/cleanup, or AI feature set.

The optional private Ubuntu companion server is implemented at application `0.1.0`, sync protocol `1.0`, baseline format `1`, project schema `8`, control schema `10`, and backup format `1`. It adds read-only NAS scanning and verified global identity, durable jobs, shared representations, independent server project replicas synchronized by operations, Chrome review/admin, revocable authentication, and verified database backups. It never shares live SQLite files or uploads originals. See [`docs/server/README.md`](docs/server/README.md).

Local automated/macOS process acceptance passed. Ubuntu/Compose, physical NAS, LAN Caddy, Tailscale, native Tauri, and target resource/thermal acceptance did not run and remain release risks; the sole evidence is [`docs/tasks/server/ACCEPTANCE_TASK_19.md`](docs/tasks/server/ACCEPTANCE_TASK_19.md). Sections below preserve the broader product target and must not be read as claiming every listed screen/capability is implemented.

---

# 1. Design Goals

The initial architecture should support:

* Fast local iteration
* macOS desktop integration
* Playback of standardized proxies
* Media stored on NAS and removable drives
* A keyboard-first review interface
* Interactive timeline calibration
* Structured access from Claude and Codex
* Deterministic timeline export
* An optional private Linux companion node without weakening desktop offline use

The design should not prematurely optimize for:

* Multi-user collaboration
* Cloud storage
* Mobile clients
* A custom embedded AI harness
* A full nonlinear editor

---

# 2. High-Level Architecture

```text
┌────────────────────────────────────────────────────────────┐
│                  RCD VlogCutter Desktop                    │
│                                                            │
│  Tauri 2                                                   │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ React / TypeScript frontend                         │  │
│  │                                                      │  │
│  │ Project library                                      │  │
│  │ Ingest                                               │  │
│  │ Review and annotation                                │  │
│  │ Timeline chronology calibration                      │  │
│  │ Trip-context editor                                  │  │
│  │ Script and edit-plan status                          │  │
│  │ Media availability                                   │  │
│  └──────────────────────┬───────────────────────────────┘  │
│                         │ Typed commands / events           │
│  ┌──────────────────────▼───────────────────────────────┐  │
│  │ Rust desktop and core                               │  │
│  │                                                      │  │
│  │ File and folder permissions                         │  │
│  │ Mounted-volume detection                            │  │
│  │ Native file dialogs                                 │  │
│  │ Per-project catalog and project state                │  │
│  │ Ingest, fingerprinting, and chronology              │  │
│  │ Review state, notes, and edit-plan validation       │  │
│  │ Managed FFmpeg / ffprobe / ExifTool processes       │  │
│  │ Sleep prevention                                    │  │
│  │ CLI and agent API                                   │  │
│  │ OpenTimelineIO export                               │  │
│  └──────────────────────────────────────────────────────┘  │
└──────────────────────────┬─────────────────────────────────┘
                           │
             ┌─────────────┼─────────────┐
             ▼             ▼             ▼
        Mounted NAS     USB SSD       SD cards
```

---

# 3. Frontend Design

## 3.1 Proposed stack

* Tauri 2
* React
* TypeScript
* Vite
* Shared type definitions
* DOM-based standard UI
* Canvas or efficient positioned elements for timeline surfaces
* HTML video playback using standardized proxies

The first timeline spike can remain DOM-based.

Canvas or WebGL should only be introduced if real-project scale causes performance problems.

---

## 3.2 Primary screens

### Project library

* Existing projects
* Recent projects
* Media availability
* Last review position
* Processing status

### Ingest

* Select one or more folders
* Preview detected cameras and cards
* Reference in place
* Generate representations
* Processing progress

### Review, annotation, and sequencing

The primary application screen.

Contains:

* Review queue
* Previous/current/next context
* Video player
* Key moments
* Notes
* Labels
* Clip details
* Raw and inferred timestamps
* Ordering confidence
* Compact five-source chronology overview

### Chronology calibration

Expanded overlay or full-window mode.

Contains:

* Trip-context band
* Time ruler, marking day and timezone changes
* Camera/card lanes
* Thumbnail clip tiles, with the ordinal always visible and the time on hover
* Compare deck: the focused clip playing, flanked by the clips placed either
  side of it in solved order
* Raw and best-local timestamps
* Card-boundary confirmation
* Selected adjustment scope
* Offset controls
* Drag interaction
* Clock boundaries
* Clock anchors
* Before/after placement
* Impact preview
* Apply or cancel

### Project metadata and agent status

* Script revision
* Edit-plan revision
* Stale-artifact warnings
* Agent workspace path
* Export history

---

# 4. Desktop and Core Rust Layer

Rust is the canonical desktop and domain implementation. Tauri commands expose typed operations to React without a permanent sidecar service.

Responsibilities:

* Open native file and folder dialogs
* Persist access to approved directories
* Detect mounted and unmounted volumes
* Own SQLite migrations and database access
* Own asset identity, ingest, project state, notes, chronology, and edit-plan validation
* Invoke and monitor FFmpeg, ffprobe, and ExifTool as managed child processes
* Prevent sleep during proxy generation or preview rendering
* Store credentials securely
* Send system notifications
* Manage application lifecycle and updates

Small Python scripts may be used during analysis experiments. Python must not become an embedded or permanently running application service.

---

# 5. Core Backend

## 5.1 Technology

Initial implementation:

* Stable Rust
* Serde
* rusqlite
* SQLite
* FFmpeg and ffprobe
* ExifTool
* Direct OpenTimelineIO JSON export when the required schema remains straightforward
* A tiny managed Python OpenTimelineIO exporter only if direct JSON cannot preserve the required sequence semantics

## 5.2 Service boundaries

The shared Rust core crate owns:

* Database access
* Asset identity
* Project state
* Media processing
* Search
* Chronology solving
* Agent operations
* Timeline validation
* Export

The React frontend never opens SQLite directly. Tauri commands and the Rust CLI call the same Rust core crate.

---

# 6. Storage Model

## 6.1 Project catalog

The desktop keeps one canonical SQLite catalog inside each project folder. It tracks desktop media facts and project state together. The optional server separately provides verified cross-project media/representation reuse while project membership and annotations remain operation-backed in each project catalog/replica.

Tracks media facts:

* Asset ID
* Content fingerprint
* Technical metadata
* Camera identity
* Card or capture sequence
* Filename counter
* Known physical locations
* Cached representations
* Transcript
* Visual summaries
* Clock anchors
* Durable annotation history

## 6.2 Project state

Tracks project-specific facts:

* Selected assets
* Review status
* Project annotation
* Labels
* Event membership
* Chapters
* Chronology constraints
* Trip-context phases
* Script revisions
* Edit-plan revisions
* Export history

## 6.3 Project workspace

The project root is a first-class workspace for general-purpose coding agents
such as Claude Code and Codex. A user should be able to open the video project
root directly in such an agent and have the agent learn what the project is and
how to work on it safely, without bespoke per-session setup. Materializing this
workspace is planned, not implemented; it is tracked by issue #48 and Milestone
5. Only `catalog.sqlite3`, `cache/`, and `exports/` exist today.

Intended project-root layout:

```text
~/Movies/VideoProjects/TripName/
├── AGENTS.md          # generated  — how an agent should work on this project
├── PROJECT.md         # generated  — compact, readable project state
├── script.md          # authored   — human/agent creative intent
├── project.json       # generated  — machine-readable project identity/config
├── edit-plan.json     # projection — transactional, revision-guarded
├── catalog.sqlite3    # canonical  — do not edit directly
├── cache/             # generated  — disposable, do not edit directly
│   └── proxies/
└── exports/           # generated  — export outputs and history
```

SQLite remains the single canonical store for catalog, project, chronology,
annotation, job, and revision state. Workspace files are a discovery and
interchange layer over it, never a competing source of truth.

Three artifact classes, with different authority:

**1. Human- and agent-authored artifacts** — `script.md`, and future creative
notes or story intent. These may be edited directly by the human or by an
agent. They are versioned inputs to VlogCutter, not projections of it.

**2. Generated read-only projections** — `AGENTS.md`, `PROJECT.md`, and
`project.json`. `PROJECT.md` carries compact agent-readable state such as clip
counts, chronology span, analysis status, unresolved issues, and the current
script and edit-plan revisions. VlogCutter owns these values and regenerates
the projection; re-projecting unchanged state produces no file diff. Editing the
Markdown must not become an alternate way of mutating canonical state.

**3. Structured transactional projections** — `edit-plan.json`. Canonical
edit-plan state stays in SQLite. The JSON file is an agent-readable projection
and interchange artifact; arbitrary edits to it do not silently mutate the
catalog. Mutation goes through revision-safe CLI operations, or through a future
explicit apply/import operation that validates the plan and rejects a write
based on a stale revision.

Originals, `cache/`, and `catalog.sqlite3` are never edited directly by an
agent. The generated `AGENTS.md` inside a video project is distinct from this
repository's root `AGENTS.md`, which is an engineering handoff for people and
agents working on VlogCutter's source code.

---

# 7. Asset Identity

## 7.1 Logical asset

A logical media item independent of file path.

Suggested fields:

```json
{
  "asset_id": "asset_7f31e2",
  "fingerprint": "…",
  "filename": "C0163.MP4",
  "duration_seconds": 18.4,
  "camera_id": "camera_sony_a7c2",
  "capture_sequence_id": "sony_sd_a"
}
```

## 7.2 Fingerprinting

Initial strategy:

1. File size
2. Technical metadata
3. Sampled-content hash
4. Optional full hash in background

The system should prioritize fast ingest while preserving a path toward strong duplicate verification.

---

# 8. Camera and Capture-Sequence Model

## 8.1 Camera

Represents the physical device when identifiable.

Possible evidence:

* Make and model
* Serial number
* Codec profile
* Resolution patterns
* Lens metadata

## 8.2 Capture sequence

Represents a likely monotonic filename chain.

Possible boundaries:

* Different SD-card volume
* Filename counter reset
* Folder reset
* Filename prefix change
* Large discontinuity
* Explicit user split

These are signals, not conclusions. A counter reset is equally consistent with a
card swap and with a camera rolling its own counter over, so the product asks
rather than assumes, and records the answer either way — a split, or a durable
"same card" that stops the question returning.

Example:

```text
Sony A7C II
├── Sony SD-A · C0101–C0244
└── Sony SD-B · C0001–C0180
```

Filename identity must include capture sequence because different cards may reuse the same counter.

---

# 9. Chronology Model

## 9.1 Time representations

Each clip may have:

* Raw video timestamp
* Optional metadata timezone
* Best-guess trip time
* Best-guess local display time
* Confidence
* Evidence list
* Uncertainty interval

The solver should use a continuous underlying trip-time axis.

Local-time display is derived from trip context.

---

## 9.2 Clock epoch

A clock epoch applies one offset model to a contiguous filename range.

Suggested representation:

```json
{
  "epoch_id": "epoch_sony_a_02",
  "capture_sequence_id": "sony_sd_a",
  "start_filename_counter": 163,
  "end_filename_counter": null,
  "offset_minutes": -480,
  "confidence": 0.86,
  "evidence": [
    "user_boundary",
    "station_clock_anchor",
    "cross_camera_event_alignment"
  ]
}
```

Clock epochs should not modify source metadata.

---

## 9.3 Event-order constraint

Represents knowledge independent of exact time.

```json
{
  "earlier_event_id": "event_breakfast",
  "later_event_id": "event_departure",
  "source": "user",
  "strength": "hard"
}
```

---

## 9.4 Individual exception

Rare override for one clip that breaks the normal camera sequence.

This should be explicit and visible because it weakens monotonic filename assumptions.

---

## 9.5 Solver responsibilities

The initial solver should:

1. Build capture-sequence chains.
2. Apply raw timestamp hypotheses.
3. Apply clock epochs.
4. Apply clock anchors.
5. Apply trip-context timezone mappings.
6. Apply user event constraints.
7. Detect contradictions.
8. Calculate best-guess trip times.
9. Produce a stable total order for review.
10. Expose confidence and evidence.

A complex optimization engine is not required initially.

A deterministic constraint graph plus scored candidate times is sufficient for the MVP.

---

# 10. Trip-Context Model

## 10.1 Purpose

Trip context describes where the user physically was.

It is distinct from camera timestamps.

## 10.2 Phase structure

```json
{
  "phase_id": "phase_canada",
  "label": "Canada",
  "type": "location",
  "start_anchor_asset_id": "asset_arrive_canada",
  "end_anchor_asset_id": "asset_leave_canada",
  "start_trip_time": "…",
  "end_trip_time": "…",
  "timezone": "America/Toronto",
  "display_mode": "local"
}
```

Transit example:

```json
{
  "phase_id": "phase_japan_canada_flight",
  "label": "Plane: Japan → Canada",
  "type": "transit",
  "start_trip_time": "…",
  "end_trip_time": "…",
  "origin_timezone": "Asia/Tokyo",
  "destination_timezone": "America/Toronto",
  "display_mode": "dual"
}
```

## 10.3 MVP editor

The MVP should support:

* Add phase
* Rename phase
* Select location or transit
* Drag start/end boundaries
* Assign timezone when known
* Select transit display mode
* Hide or show band

Phase boundaries can remain rough.

Ribbon-created phases persist their first and last clip identities as boundary
evidence. The UTC bounds remain as a fallback projection, but ordinary solving
resolves the anchors again so a later correction to another camera moves its
clips into or out of the phase without detaching the phase from the footage the
user chose. A missing, unplaced, or crossed anchor retains the last resolved
range and produces an explicit chronology conflict rather than silently
swapping boundaries.

---

# 11. Clock-Anchor Model

## 11.1 Detection

Background analysis may propose anchors from:

* Analog clocks
* Digital clocks
* Phone screens
* Tickets
* Receipts
* Departure boards

## 11.2 Confirmation

The user sees:

* Keyframe
* Detected reading
* Proposed timezone or phase
* Confidence
* Suggested offset impact

The user may:

* Confirm
* Correct reading
* Mark unusable
* Select exact frame

## 11.3 Stored anchor

```json
{
  "asset_id": "asset_station_clock",
  "source_time_seconds": 6.2,
  "observed_local_time": "08:47",
  "trip_context_phase_id": "phase_japan",
  "uncertainty_seconds": 60,
  "source": "user_confirmed_visual_clock"
}
```

---

# 12. Review-Queue Stability

Chronology changes may reorder many clips.

To avoid disorientation:

* Capture the next intended review item before calibration.
* Apply chronology correction.
* Recompute the queue.
* Resume at the previously intended next item when possible.
* Otherwise choose the next unreviewed item.

The UI should explain when a correction moves many clips.

Example:

```text
24 GoPro clips recalibrated.
The review queue was updated.
Continuing with the next previously queued clip.
```

---

# 13. Media Representations

Each asset may have:

* Original
* Review proxy
* Contact sheet
* Individual keyframes
* Extracted audio
* Waveform
* Transcript
* VLM analysis
* Embedding

## Thumbnail quality

Thumbnail quality is an integer contract:

* `0`: no usable thumbnail
* `1`: basic deterministic thumbnail, implemented in the MVP as a 480-pixel-wide JPEG sampled at 20% of the clip
* `2–9`: reserved
* `10`: reserved for a future VLM-chosen frame

Generation is user-triggered because source decoding across thousands of 4K/60 clips can take minutes. The original is preferred, with an available proxy as the offline fallback. Files are preserved by level under `<project>/cache/thumbnails/<asset-id>/q<quality>.jpg`, and the UI displays the highest available quality.

## Default review proxy

Initial spike recommendation:

* H.264
* AAC
* 720p
* Moderate bitrate
* Preserve aspect ratio
* Convert problematic HDR material to reviewable SDR where necessary

Original-media decoding should remain outside the webview when practical.

---

# 14. Search

The MVP should support text search over:

* User notes
* Prior-project annotations
* Transcripts
* VLM summaries
* Events
* Chapters
* Filenames
* Camera/card sequences

Search is useful for agent queries and navigation.

It should not be the primary chronology-correction interaction.

---

# 15. CLI and Agent API

The agent surface has two complementary halves. The project workspace of
Section 6.3 is the *discovery, context, and bootstrap* layer: it is how an
arbitrary coding agent, opened cold at a project root, learns that this is a
VlogCutter video project, what it contains, which files it may edit, and that
this CLI exists. The CLI is the *query, mutation, and control* layer: it is the
authoritative transactional interface to canonical SQLite state, with stable
JSON envelopes, explicit project selection, and revision checks.

Neither replaces the other. The workspace is not optional, and it is not an
alternative to the CLI. An MCP server is not required for the MVP; see
Section 25.1.

Implemented MVP commands:

```bash
rcdvlog doctor
rcdvlog project create --name "Expo 2025"
rcdvlog project list
rcdvlog project status --project project_123

rcdvlog ingest --project project_123 --folder /Volumes/data/Expo

rcdvlog clips list --project project_123 --query arrival --limit 20
rcdvlog clips get --asset asset_7f31e2
rcdvlog clips update --asset asset_7f31e2 --notes "Arrival" --add-label arrival
rcdvlog clips update --asset asset_7f31e2 --removed true

rcdvlog clock apply --project project_123 --camera "DJI Pocket 3" \
  --start-filename DJI_0001.MP4 --offset-minutes -60

rcdvlog proxy generate --asset asset_7f31e2
rcdvlog proxy missing --project project_123
rcdvlog thumbnails generate --asset asset_7f31e2 --quality 1
rcdvlog thumbnails missing --project project_123 --quality 1
```

The CLI retains the legacy application-support catalog as its no-argument fallback. Agents use `--catalog <project folder>/catalog.sqlite3` or `RCDVLOG_CATALOG` to select a desktop project explicitly.

CLI requirements:

* One JSON success envelope on stdout
* Machine-readable JSON errors on stderr with nonzero exit status
* JSON progress events on stderr for long ingest commands
* Explicit project selection
* No interactive prompts in agent mode
* Patch-style clip updates that preserve omitted fields
* Shared Rust domain behavior with the desktop app
* No silent source-media mutation

Transcript, keyframe, script-revision, semantic-search, and preview-render CLI commands are deferred with their underlying post-MVP features.

Edit-plan commands and workspace projection do not exist yet. Both are tracked
by issue #48 and specified in Milestone 5. When they land, workspace
projections remain projections: writing to `PROJECT.md` or `edit-plan.json` by
hand is not equivalent to a CLI mutation, and importing a changed projection is
an explicit, validated, revision-guarded operation.

---

# 16. Project Concurrency and Synchronization

Each MVP project has one canonical local SQLite source of truth at `<project folder>/catalog.sqlite3`. Desktop and CLI processes may concurrently open that catalog in WAL mode.

Possible concurrent surfaces:

* Desktop UI
* Claude
* Codex
* CLI commands

Important mutable artifacts carry revision identifiers.

Examples:

```text
Current script revision: 15
Edit plan generated from script revision: 12
Status: stale
```

Initial conflict behavior:

* Simple fields: latest committed revision
* Notes: preserve revision history
* Scripts: preserve all revisions
* Edit plans: preserve all revisions
* Chronology calibration: explicit domain operations
* Generated outputs: never silently replace newer active output

Implemented cross-device behavior uses immutable typed project operations, HLC ordering, deterministic materialized views, per-origin sequence cursors, explicit conflict diagnostics, and bounded at-least-once reconciliation between independent local SQLite files. Network failure never rolls back a committed desktop mutation. Live SQLite/WAL files and absolute paths are not synchronization artifacts.

Transport-level optimistic revision tokens for competing desktop/CLI full-patch writes remain deferred. Multi-user collaboration and multiple independent Mac editing origins are also out of scope.

---

# 17. Processing Jobs

Long-running work should be represented as resumable jobs. The companion server implements durable jobs with attempts, dependencies, leases, retry/cancel, crash recovery, schedules, events, and persisted node-scoped throughput. Desktop proxy/thumbnail batches still commit outputs incrementally but are not backed by the server job table.

Examples:

* Scan folder
* Fingerprint asset
* Generate proxy
* Generate contact sheet
* Extract metadata
* Transcribe
* Analyze keyframes
* Detect clock
* Render preview
* Export timeline

Job fields:

* Job ID
* Project or asset ID
* Input representation
* Settings
* Progress
* Output identity
* Retry state
* Error details
* Version

The UI should allow annotation work to begin before all jobs finish.

---

# 18. Repository Structure

The implemented workspace contains `apps/desktop`, `apps/cli`, `apps/server`, and `apps/web`; Rust domain crates under `core/` for desktop/project behavior, synchronization, server domains, and batch estimation; shared TypeScript packages under `packages/`; production assets under `deploy/server`; and current operator/developer guidance under `docs/server`. The detailed operational map lives in [`AGENTS.md`](AGENTS.md).

---

# 19. Optional Companion Server

The desktop does not depend on Linux. The implemented companion server can index configured read-only NAS roots without the Mac, verify global content identity, generate shared proxies/thumbnails, host operation-synchronized project replicas, serve selected representations to Chrome, download compatible proxies to the Mac, and queue node-targeted representation requests. It also provides private authenticated web review/admin and verified database backups.

The server is the durable synchronization hub, not the exclusive project owner. Desktop and server databases stay local to their node; they exchange versioned operations and path-free facts. Original bytes never transfer in this epic. VLM analysis, bulk “prepare offline,” multi-user collaboration, and high availability remain deferred.

The desktop remains usable while the node is unavailable.

---

# 20. Main Technical Risks

## Timeline performance

Mitigation:

* Begin with efficient DOM positioning.
* Virtualize lanes and clips if necessary.
* Move to Canvas only after profiling.

## WebKit media limitations

Mitigation:

* Standardize review proxies.
* Use FFmpeg for source decoding.
* Preserve the option for native AVFoundation playback later.

## Chronology-model complexity

Mitigation:

* Model capture sequences and clock epochs explicitly.
* Prioritize transparent rules over opaque optimization.
* Keep individual exceptions rare and visible.

## Incorrect clock detection

Mitigation:

* Treat automatic anchors as suggestions.
* Require confirmation for strong changes.
* Store confidence and provenance.

## Scope expansion into full editing

Mitigation:

* Keep Resolve as the finishing environment.
* Focus on metadata, chronology, scripting, and rough-cut export.

---

# 21. MVP Scope Contract

The MVP is a single-user macOS application that takes media from mounted folders through ingest, proxy review, chronology correction, annotation, a machine-readable edit plan, preview rendering, and a timeline handoff to the finishing editor.

The MVP is successful when one real trip project can be completed through that loop without editing the database or project JSON by hand.

## 21.1 Required user journey

1. Create or open a project.
2. Add one or more media folders from a NAS, removable drive, or local disk.
3. Inspect detected assets and capture sequences before committing the ingest.
4. Reference originals in place and generate local quality-1 thumbnails and review proxies.
5. Begin reviewing available clips while remaining processing jobs continue.
6. Add free-form labels and notes with keyboard-first controls, and reversibly remove unusable clips from this view.
7. Inspect the proposed chronology and correct a camera or filename range with a clock epoch.
8. Add trip-context phases and verify how local time is displayed.
9. Resume the review queue without losing the next intended item after a chronology change.
10. Search project metadata and expose project state through the CLI.
11. Create and validate a versioned script and edit plan without silently replacing newer work.
12. After labeling is complete, create an edit plan, render a low-resolution preview, and export a deterministic timeline for the finishing editor.
13. Close and reopen the application with project state, job state, and review position intact.
14. Disconnect and reconnect a media volume without corrupting the project.

## 21.2 Launch-critical capabilities

* Project library and project creation
* Reference-in-place ingest
* Stable asset identity and physical-location tracking
* Metadata extraction and capture-sequence proposals
* Resumable job execution
* Standardized review proxies and deterministic quality-1 thumbnails
* Keyboard-first playback, free-form labeling, notes, and reversible clip removal
* Stable review ordering and resume behavior
* Manual trip-context phases
* Manual clock epochs, explicit clip exceptions, and contradiction reporting
* Explainable best-time calculation with confidence and evidence
* Text search over metadata available in the project
* Versioned project workspace artifacts that make the project root self-describing to a general-purpose coding agent (Section 6.3)
* Non-interactive JSON CLI for project, clip, chronology, script, and edit-plan operations
* Edit-plan validation
* Preview rendering
* Deterministic timeline export
* Missing-volume detection and relinking

## 21.3 Post-MVP enrichments

These capabilities are intentionally excluded from the MVP so implementation can validate the ingest, reorder, review, notes, and export loop:

* Automatic transcription
* VLM summaries
* Automatic visual clock detection
* Embedding or semantic search
* Automatic scene detection
* Background full-file hashing after the fast fingerprint is available

The core review and chronology flows must remain useful without them. No permanent Python service or model-provider dependency should be introduced in anticipation of this later work.

## 21.4 Explicitly deferred

* Multi-user collaboration and multiple independent Mac editing origins
* Cloud media storage
* Mobile clients and original-media transfer; the private Chrome review/admin client is implemented
* Fine-cut editing, transitions, effects, color, or audio mixing
* General-purpose timeline editing
* Automatic chronology changes that are applied without user confirmation
* A custom agent runtime or chat interface inside the desktop app
* Extension marketplace or plugin system

---

# 22. Proposed Runtime and Contracts

These are the accepted MVP implementation defaults.

## 22.1 Process model

* React calls typed Tauri commands implemented in Rust.
* Tauri commands and the CLI share one Rust core crate containing all database and domain behavior.
* SQLite uses WAL mode and explicit revision checks for safe desktop and CLI access.
* FFmpeg, ffprobe, and ExifTool run as managed child processes; their output and exit state are captured by the Rust core.
* Long-running proxy operations report progress through Tauri events and persist their result state.
* Batch progress uses a reusable weighted estimator with persisted per-task throughput history. Proxy and thumbnail weights are source bytes. macOS missing-proxy batches use up to four local workers; single-proxy calls, thumbnails, and the server/non-macOS proxy path use one. Future cloud inference supplies its own cost weights and worker concurrency. ETA is the estimated slowest worker shard, not a naive division by worker count.
* A tiny, short-lived Python process may be used for OpenTimelineIO export only if direct JSON export proves insufficient.
* The desktop has no permanent loopback sidecar or embedded Python runtime. The optional companion server is a separate private Linux process with a loopback-only backend.

This keeps domain behavior identical across the desktop and CLI while minimizing the MVP's process and packaging surface.

## 22.2 Source-of-truth rules

* SQLite is canonical for catalog, project, chronology, annotation, job, and revision state.
* Originals are immutable. Non-removable sources are referenced in place. The
  accepted removable-card workflow may copy from a read-only card into a named
  create-only archive root; it never moves, rewrites, renames, deletes, or
  overwrites original media.
* Generated representations are disposable, versioned cache artifacts.
* Workspace files are stable projections or versioned inputs, not an independent competing database.
* Importing a changed workspace artifact requires explicit validation and creates a new revision.
* All mutations are committed as named domain operations with an operation ID, timestamp, source, and expected base revision.
* Repeating a command with the same operation ID is idempotent.

## 22.3 Initial local storage layout

```text
~/Movies/VideoProjects/
└── <project-name>/
    ├── catalog.sqlite3
    ├── cache/
    │   └── proxies/
    └── exports/
```

This is the layout that exists today. The agent-facing workspace artifacts of Section 6.3 — `AGENTS.md`, `PROJECT.md`, `script.md`, `project.json`, and `edit-plan.json` — are planned and not yet materialized.

The parent project folder defaults to `~/Movies/VideoProjects` and is configurable in global desktop settings. Project directories use a filesystem-safe form of the project name with numeric suffixes for collisions. The first-run screen scans immediate child folders for valid catalogs. Global playback settings offer autoplay modes of never, only when a proxy is available, or always; proxy-only autoplay is the default and existing settings files migrate to it.

## 22.4 Contract generation

* Serde models define command, event, workspace, and edit-plan contracts.
* `specta` or an equivalent generator may produce TypeScript types after the first vertical slice.
* Every persisted schema and job payload carries a version.
* Contract tests verify frontend fixtures until generated TypeScript contracts are introduced.

---

# 23. MVP Implementation Plan

The plan is organized as vertical slices. Each milestone must leave the application in a demonstrable, testable state. Calendar estimates should be added only after team size, target media scale, and the optional enrichment scope are decided.

## Milestone 0 — Acceptance fixtures and engineering skeleton

Build:

* Create the repository structure from Section 18.
* Pin supported Node, Rust, FFmpeg, ffprobe, and ExifTool versions. Pin Python only if the OTIO fallback is selected.
* Add formatting, linting, type checking, unit tests, and packaging checks to CI.
* Establish SQLite migrations and shared Serde contracts, with TypeScript contract fixtures or generation.
* Create synthetic and redistributable media fixtures covering multiple cameras, duplicate filenames, timestamp offsets, a counter reset, a missing volume, and at least one HDR source.
* Capture a small anonymized real-project acceptance fixture or fixture manifest.
* Add structured logs with operation and job IDs.

Exit criteria:

* Tauri initializes the Rust core and catalog in development.
* React can execute one typed Tauri command and receive its result.
* One migration can be applied to a new database and rolled forward from the prior schema version.
* CI runs on a clean checkout with no globally installed project tools other than the documented bootstrap prerequisites.

## Milestone 1 — Project library, catalog, and durable jobs

Build:

* Define project, asset, physical location, representation, camera, capture sequence, and job records.
* Implement project create, open, list, archive, and recent-position operations.
* Implement the project library UI and basic processing-status display.
* Implement a persistent job queue with leases, progress, cancellation, retry, error details, and recovery after process termination.
* Implement mounted-volume inventory and media-availability states.
* Establish cache paths and representation versioning.

Exit criteria:

* A user can create, close, reopen, and archive a project.
* A deliberately interrupted test job resumes or restarts safely after application relaunch.
* Disconnecting a fixture volume marks affected assets unavailable without deleting catalog records.
* The same project summary is returned by the desktop and CLI application layers.

## Milestone 2 — Ingest and representation pipeline

Build:

* Add native folder selection and persisted directory access.
* Scan folders without following unsafe cycles or escaping approved roots.
* Extract technical and timestamp metadata with ffprobe and ExifTool while preserving raw values and provenance.
* Implement the fast fingerprint and duplicate-candidate workflow.
* Propose cameras and capture sequences using volume, folder, prefix, counter, metadata, and discontinuity evidence.
* Present an ingest preview and allow explicit sequence splits or merges before commit.
* Generate versioned H.264/AAC review proxies, contact sheets, and selected keyframes.
* Surface per-asset progress and actionable failures.

Exit criteria:

* Re-ingesting the same fixture does not create duplicate logical assets.
* Two different cards that both contain `C0001.MP4` remain distinct and correctly sequenced.
* Proxy generation is deterministic for the same source and settings version.
* A failed transcode can be retried without repeating successful work.
* Review can begin while other representations are still queued.

## Milestone 3 — Keyboard-first review and annotation

Build:

* Implement the review queue, previous/current/next context, proxy playback, and last-position persistence.
* Define the initial keyboard map and an in-app shortcut reference.
* Implement free-form labels, notes, and reversible queue removal.
* Implement source-time key moments with revision history after the basic labeling loop is validated.
* Show technical metadata, raw timestamps, camera, sequence, media availability, and processing state.
* Implement queue filtering and basic SQLite FTS search over filename, notes, labels, and any available transcript or analysis text.
* Add optimistic concurrency checks to annotation mutations.

Exit criteria:

* A user can review and annotate the full fixture without using a mouse except where text entry requires focus.
* Relaunch resumes the last project, intended queue item, and playback position.
* An annotation conflict never silently discards either committed revision.
* Search results identify which field matched and navigate to the asset.

## Milestone 4 — Chronology and trip-context calibration

Build:

* Define the continuous trip-time representation and local-display derivation.
* Implement deterministic capture-chain ordering and stable tie-break rules.
* Store raw timestamp hypotheses separately from derived best times.
* Implement clock epochs over contiguous capture-sequence ranges.
* Implement individual clip exceptions, clock anchors, event-order constraints, and contradiction detection.
* Implement trip-context location and transit phases with timezone and display-mode controls.
* Build the compact chronology view and expanded calibration view.
* Add drag and numeric offset editing with before/after impact preview and explicit apply/cancel.
* Explain each asset's time, confidence, evidence, and applicable corrections.
* Preserve the intended next review item when a correction reorders the queue.

Exit criteria:

* The chronology fixture produces the same total order on repeated runs and across desktop and CLI calls.
* Applying one epoch changes only its declared filename range and dependent derived data.
* A hard contradictory constraint blocks apply and identifies the conflicting evidence.
* A calibration that moves many assets previews the impact and resumes at the captured next item.
* No operation mutates source metadata.

## Milestone 5 — Agent workspace, CLI, and revision safety

Tracked by issue #48, "Agent workspace + edit-plan CLI: make projects
self-describing to Codex/Claude".

Build:

* Materialize `AGENTS.md`, `PROJECT.md`, `script.md`, `project.json`, and `edit-plan.json` from a versioned workspace template, respecting the Section 6.3 authority classes.
* Generate a project-local `AGENTS.md` that teaches a coding agent, with no prior VlogCutter knowledge, that this is a video project rather than a source tree; what the project contains; which files are generated, authored, or transactional; how to inspect clips, chronology, transcripts, VLM observations, representative media, and edit-plan state; how to use the CLI; how revisions and stale-write protection work; and that originals, caches, and `catalog.sqlite3` must never be edited directly.
* Implement the Section 15 CLI surface with explicit project selection, stable JSON envelopes, and machine-readable error codes.
* Add chronology explain and project status responses suitable for external agents.
* Implement script and edit-plan revision creation, validation, activation, and stale-dependency reporting.
* Require expected revision IDs for mutations that can replace active artifacts.
* Make workspace projection writes atomic.
* Document example Claude and Codex workflows using only the CLI and workspace files.

Agents should reason from VlogCutter's derived knowledge — project status,
chronology, labels, notes, transcripts, analysis summaries — plus targeted
access to representative media such as thumbnails, keyframes, or a specific
proxy range. Ingesting all original video into an agent's context is not the
intended design and must not be required to complete the cold-start journey.

Exit criteria:

* Cold-start CUJ: a fresh Claude Code or Codex session opened at a project root, with no user-supplied setup instructions beyond the natural-language task, can discover from the project-local `AGENTS.md` how to inspect the project, find the relevant clips for a request such as "make a 3-minute sequence covering our arrival in Madrid", and create a new edit-plan revision through the CLI without touching originals, caches, or the catalog file directly.
* Every documented CLI command has a JSON contract test and exits nonzero with a stable error code on failure.
* Two edits based on the same script revision cannot silently replace one another.
* An edit plan derived from an older script is visibly stale in the UI and CLI.
* Re-projecting unchanged workspace state produces no file diff.
* Hand-editing a generated projection does not change canonical state; importing a changed transactional projection is rejected when its base revision is stale.

## Milestone 6 — Edit plan, preview, and finishing handoff

Build:

* Specify edit-plan operations, including source asset, source range, timeline placement, track, rationale, and originating script revision.
* Validate asset existence, media availability, range bounds, overlaps, unsupported operations, and revision freshness.
* Render a low-resolution preview through FFmpeg with cancellable, resumable job semantics where practical.
* Export deterministic OpenTimelineIO with stable clip and metadata identifiers.
* Add export history and prevent silent replacement of newer outputs.
* Validate the real finishing-editor handoff and document any adapter or conversion step.

Exit criteria:

* Invalid ranges and missing assets fail before rendering with actionable errors.
* The fixture edit plan renders an audiovisual preview with expected duration and clip order.
* Re-exporting identical inputs produces semantically identical OTIO.
* The exported timeline opens in the agreed finishing workflow with source references, in/out points, ordering, and clip names intact.

## Milestone 7 — Optional analysis adapters

Build only the enrichments selected for the MVP:

* Transcription adapter and transcript indexing
* VLM adapter, prompt/version provenance, cost accounting, and summary indexing
* Visual clock-anchor proposals with user confirmation
* Scene detection
* Embedding generation and semantic search

Exit criteria:

* Every external analysis result records provider, model, settings, input identity, output version, and error details.
* Disabling credentials or network access degrades gracefully to the complete manual workflow.
* Suggested clock anchors never alter chronology until explicitly confirmed.

## Milestone 8 — Hardening and release candidate

Build:

* Test interrupted ingest, corrupt media, disk-full behavior, cache deletion, source moves, volume renames, managed-process failures, and application upgrades.
* Add relink by fingerprint and explicit handling for ambiguous matches.
* Profile project-open, search, queue recomputation, and timeline interaction at the agreed target scale.
* Add database backup before migration and a support-bundle exporter that excludes credentials.
* Package the required media tools or implement a documented prerequisite check.
* Complete macOS signing, notarization, permissions messaging, and update behavior if required for the chosen distribution model.
* Run the full acceptance journey on a clean target Mac and a real trip project.

Exit criteria:

* No known data-loss or silent-overwrite defect remains.
* A failed migration leaves a recoverable database and a useful error.
* The target-scale project meets the agreed performance budgets.
* The clean-machine acceptance run completes without developer tooling.
* All launch-critical acceptance criteria in Section 24 pass.

---

# 24. MVP Acceptance and Quality Plan

## 24.1 End-to-end acceptance scenarios

### A. Happy-path trip project

Ingest at least three capture sequences from two cameras, review and annotate clips, apply one clock epoch, add a transit phase, generate an edit plan, render a preview, and export the finishing timeline.

### B. Duplicate identity and reconnect

Ingest duplicate filenames from different cards, disconnect the source volume, continue working from proxies, reconnect or relink the source, and export without changing logical asset IDs.

### C. Interrupted processing

Terminate the app during scan, proxy, and preview jobs. Relaunch and verify that completed outputs are reused and incomplete work is safely resumed or retried.

### D. Chronology contradiction

Create conflicting hard constraints. Verify that the system explains the conflict, refuses an invalid apply, and preserves the prior stable order.

### E. Concurrent artifact revisions

Create two script or edit-plan revisions from the same base through different surfaces. Verify that both are preserved, neither silently replaces the other, and stale state is visible.

### F. Missing and corrupt media

Include unreadable media, unsupported streams, missing originals, and insufficient cache space. Verify that failures are isolated per asset or job and the rest of the project remains usable.

## 24.2 Test layers

* Domain unit tests for fingerprints, sequence boundaries, time derivation, constraints, validation, and revision rules
* Property tests for deterministic ordering, idempotent operations, and time-range invariants
* Migration tests from every released database schema
* Contract tests shared by Rust and TypeScript clients
* Integration tests using real ffprobe, ExifTool, and FFmpeg fixture invocations
* React component tests for review and calibration state
* Desktop end-to-end tests for native folder access, Tauri command lifecycle, relaunch, and volume changes
* Golden-file tests for workspace projections, edit-plan validation, and OTIO semantic output
* Manual finishing-editor import test for each release candidate

## 24.3 Performance budgets to establish

The owner must provide a representative project scale before numeric budgets are frozen. At minimum, measure:

* Time to first ingest preview
* Time until the first asset is reviewable
* Warm and cold project-open time
* Review next/previous response latency
* Search latency
* Chronology impact-preview latency
* Queue recomputation latency
* Timeline pan and zoom frame rate
* Proxy throughput and cache growth
* Preview render speed relative to timeline duration

## 24.4 Release gates

* Required acceptance scenarios pass on a clean Mac.
* Schema migrations, generated contracts, and packaged runtime versions are reproducible.
* There are no open critical data integrity, security, or source-media mutation defects.
* Known unsupported media and finishing-editor limitations are documented in-product.
* Cache artifacts can be deleted and rebuilt without losing user-authored work.
* The application is fully useful without network or AI services.

---

# 25. Product and Architecture Decisions

The following decisions were accepted for the MVP on 2026-07-18.

## 25.1 Accepted MVP decisions

1. **Audience:** one known user on one Apple Silicon Mac. Public distribution, auto-update, telemetry, and generalized onboarding are not MVP requirements.

2. **Finishing editor:** target the latest DaVinci Resolve, currently Resolve 21, while retaining compatibility with the installed Resolve 20 where practical. Sequence OTIO import is proven. Resolve dropping nonessential metadata such as names and colors is accepted for this MVP; correct media references, order, and source ranges are required.

3. **Scale:** approximately 100 source hours across thousands of clips and five cameras. Typical media is 4K/60 from Sony, DJI, iPhone, Insta360, and similar devices.

4. **Source formats:** actual Sony, DJI, iPhone, and Insta360 files form the acceptance set. H.264/H.265 MP4/MOV at up to 4K/60 are the baseline; other ffprobe-readable formats are best effort with explicit per-file errors.

5. **Proxy storage:** originals remain on NAS or local storage; footage on a removable camera card is copied into a named archive root before the card disappears. Proxies live on the laptop until project completion. Review, notes, search, and chronology should work offline from proxies whenever possible. MVP cleanup is explicit rather than automatic eviction.

6. **Trip time:** a datetime. The core stores normalized RFC 3339 datetimes and preserves raw camera timestamp text. Camera/timezone change events apply a fixed offset to a contiguous filename range; local display can be derived later from trip context.

7. **Chronology corrections:** fixed offsets plus explicit clip exceptions are sufficient. Clock drift is out of scope. The principal boundaries are camera clock changes and timezone-change events.

8. **Edit plan:** one video track, linked source audio where available, and hard cuts only.

9. **Persistence:** use one portable SQLite catalog per project. New projects default to `~/Movies/VideoProjects/<project name>/`, with the parent folder configurable and laptop-local proxies stored beside the catalog.

10. **AI analysis:** transcription, VLM summaries, semantic search, and automatic clock detection are excluded from the MVP. The purpose is to validate importing NAS clips into a project, ordering them, reviewing proxies, adding notes, and exporting a rough sequence.

11. **Implementation:** React/TypeScript UI with a canonical Rust desktop and core. FFmpeg, ffprobe, and ExifTool are managed children. Python is allowed only for small exploratory analysis scripts or a short-lived OTIO exporter if direct JSON is insufficient.

12. **Labeling workflow (accepted 2026-07-20):** the initial footage pass uses free-form labels and notes, with a reversible `removed` flag for unusable clips. Review-status states, rough-cut inclusion, source ranges, and timeline export do not appear in this phase. Rough-cut decisions begin in the later edit-planning stage after clips are labeled.

13. **Companion topology (accepted 2026-07-28):** the optional private server is a synchronization hub and compute/archive index, not an exclusive project owner. Each node keeps SQLite on its own local disk and exchanges path-free operations/facts over authenticated HTTPS. The desktop remains offline-capable.

14. **Server storage and network:** original scan roots are immutable and read-only. Derived media and backups use distinct writable roots. The backend binds loopback and is exposed only through trusted LAN Caddy or Tailscale Serve; public Funnel is forbidden.

15. **Project creation and travel successor:** current project creation is desktop-only, but that is no longer permanent product law. The planned [Travel Ingest T5](docs/tasks/travel/TASK_T5_server_origin_projects_and_desktop_adoption.md) adds server-origin project creation and desktop adoption. Its planned archive is a distinct create-only writable logical root; existing original scan roots remain immutable.

16. **Trip-context boundaries (accepted 2026-08-09):** ribbon-created trip phases are anchored to specific clips, inclusive at both ends. The phase follows those clip identities when later camera chronology corrections change solved order. A person may cut a new phase through or from any clip inside an existing phase; saving the new side and shortening/removing the old side is one atomic local operation batch, while cancelling writes nothing. Fixed UTC bounds remain supported for existing and explicitly time-based phases; story beats remain a separate unresolved range model.

17. **Removable-card import (accepted 2026-08-09):** removable cards are
    read-only sources copied by the desktop into a named create-only archive
    root after a non-mutating, project-scoped preflight. Non-removable folders
    remain referenced in place. Copy verification uses full-file BLAKE3;
    identity/relink uses the sampled fingerprint. Archive relocation is one
    logical-root rebind. The app never formats, deletes, or claims a card is
    safe to erase. See `docs/tasks/import/ARCHITECTURE.md`.

18. **Agent architecture (accepted 2026-08-20):** the MVP agent architecture is
    a project-local workspace layered over the revision-safe `rcdvlog` CLI, with
    SQLite remaining canonical. The video project root is a first-class
    workspace: a generated project-local `AGENTS.md` and `PROJECT.md` make the
    project self-describing to a general-purpose coding agent such as Claude
    Code or Codex, and the CLI is the authoritative transactional interface for
    reading and mutating canonical state. These are complementary layers, not
    alternatives, and the workspace is not optional. A custom embedded AI
    harness is not required. An MCP server is not required for the MVP; revisit
    it only if a specific capability cannot be served cleanly through the
    workspace-plus-CLI model. Reads and version-creating writes are allowed
    without ceremony; activation and destructive changes require explicit
    revision checks. Workspace authority classes are defined in Section 6.3.
    Implementation is tracked by issue #48 and Milestone 5; none of it ships
    today.

## 25.2 Remaining reversible workflow questions

1. **Is lexical search sufficient for the MVP?** Proposed MVP behavior: filename and notes filtering first; SQLite FTS can follow after the review loop is validated.

2. **Resolved 2026-08-09 — same physical media at multiple paths:** retain
   multiple verified node-local locations, prefer a verified reachable
   location, and repair an offline asset when its sampled fingerprint is found
   elsewhere. Paths remain node-local and are never synchronized as project
   operations.

3. **How are events and chapters created?** Proposed MVP behavior: defer both. Notes and chronological order are sufficient for the first workflow validation.

4. **How should project deletion and cache cleanup work?** Proposed MVP behavior: no destructive project deletion in the UI; proxy cleanup is an explicit project action added after the main loop is validated.
