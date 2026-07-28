# Historical companion-server design notes

> Historical pre-implementation exploration. Current contracts live in [`docs/server`](../../server/README.md), `AGENTS.md`, and the implemented reference architecture. Do not treat tentative recommendations below as shipped behavior.

The cleanest model is:

> **The desktop is local-first and interaction-first. The server is archive-first and compute-first. The web app is a server-backed secondary surface.**

Do not make either machine the exclusive owner of the project. Make the server the **durable synchronization hub**, while the laptop remains fully usable without it.

# Recommended conceptual architecture

```text
                     ┌─────────────────────────┐
                     │      Web browser        │
                     │                         │
                     │ Review, annotate,       │
                     │ calibrate, monitor jobs │
                     └────────────┬────────────┘
                                  │ HTTPS / WebSocket
                                  ▼
┌─────────────────────┐    ┌─────────────────────────┐
│ Mac desktop         │    │ Home server             │
│                     │    │                         │
│ Local project copy  │◀──▶│ Durable project replica │
│ Local asset catalog │sync│ NAS archive catalog     │
│ Interactive UI      │    │ Proxy/background jobs  │
│ Travel media        │    │ Web application API     │
│ Resolve export      │    │ Shared proxy store      │
└─────────────────────┘    └────────────┬────────────┘
                                       │
                                       ▼
                                  NAS originals
```

The browser does not synchronize directly with the laptop. It writes to the server’s project replica, and the laptop synchronizes with the server.

That gives you a simple hub-and-spoke topology without making the desktop dependent on the hub.

# Role division

## Mac desktop: primary interactive surface

The desktop should own tasks that benefit from immediacy, attached storage, and native integration:

* Create and open projects
* Ingest from SD cards and USB drives
* Review and annotate clips
* Perform chronology calibration
* Edit trip context
* Work completely offline
* Maintain the current travel working set
* Run lightweight or interactive jobs
* Open Claude/Codex agent workspaces
* Generate the final Resolve export
* Prepare a project for offline use

The desktop has its **own local SQLite database and cache**. It never makes synchronous server calls just to save a note or move a chronology segment.

## Home server: archive and compute surface

The server should own tasks that benefit from stable storage and continuous availability:

* Scan NAS folders in place
* Maintain authoritative knowledge of what is physically on the NAS
* Generate proxies, contact sheets, transcripts, and VLM metadata
* Run long jobs while the laptop is asleep or away
* Store a durable replica of project operations
* Store shared derived representations
* Serve the web application
* Stream review proxies to browsers
* Act as the synchronization rendezvous point
* Eventually prepare assets for download to the laptop

The server is authoritative about **its own storage availability**, but not automatically authoritative about human project decisions.

## Web application: server-backed secondary surface

The browser should initially support:

* Browse projects and archived footage
* Review server-hosted proxies
* Add and revise notes
* Apply labels, favorite, and reject states
* Perform timeline calibration when proxies are available
* Edit trip context
* Review scripts and edit plans
* Monitor and launch server jobs
* Request that proxies be prepared
* Inspect sync and media availability

The browser does **not** need offline-first support for MVP. It can be a conventional server-backed application.

This is an important simplification:

```text
Desktop = local-first client
Web app = online client of the home server
Server = sync hub and compute node
```

Should offline browser editing become important later, Automerge or another CRDT system becomes more attractive. Automerge is specifically designed for independently editable local copies that merge when connectivity returns, but that complexity is unnecessary for the first server-backed web surface. ([Automerge][1])

# Do not synchronize SQLite files

Each node should own a separate local database:

```text
Mac:
~/Library/Application Support/RCDVlog/catalog.sqlite

Server:
/var/lib/rcdvlog/catalog.sqlite
```

Do not put the database itself in Syncthing, Dropbox, the NAS, or a network filesystem. SQLite’s WAL mode requires participating processes to be on the same host and does not support multi-machine access over a network filesystem. ([SQLite][2])

Instead, synchronize **domain operations** between the two local databases.

# Separate the system into five synchronization classes

This makes the design much easier to reason about.

## 1. Human project state: replicated operations

This includes:

* Notes
* Labels
* Review status
* Favorites and rejection
* Event assignments
* Clock epochs
* Chronology calibration
* Trip-context phases
* Active script revision
* Active edit-plan revision

These should synchronize as immutable domain operations.

Examples:

```json
{
  "op_id": "01JZ...",
  "device_id": "macbook",
  "device_sequence": 4182,
  "logical_time": "2026-07-20T21:04:17.351+09:00-0003",
  "project_id": "argentina-trip",
  "type": "clip_note_revision_created",
  "payload": {
    "asset_id": "asset_123",
    "note_revision_id": "note_456",
    "text": "This was before leaving the hotel."
  }
}
```

```json
{
  "type": "clock_epoch_updated",
  "payload": {
    "capture_sequence_id": "sony_sd_b",
    "start_counter": 163,
    "offset_minutes": -480,
    "base_revision": 7
  }
}
```

Each node materializes those operations into its own SQLite views.

## 2. Observed asset facts: merged

These include:

* Asset fingerprints
* File locations
* File sizes
* Camera metadata
* Capture-sequence membership
* Last-seen status
* NAS presence
* USB-drive presence

These are observations rather than user edits.

Example:

```text
Mac observed:
asset_123 exists on /Volumes/TravelSSD/...

Server observed:
asset_123 exists on /mnt/nas/archive/...
```

Both locations should be retained.

The server is authoritative only for observations about server-accessible storage. The laptop is authoritative only for observations about laptop-accessible storage.

## 3. Derived analysis: immutable and reusable

These include:

* Contact sheets
* Proxies
* Transcripts
* VLM descriptions
* Embeddings
* Waveforms
* OCR and detected clocks

Treat each result as immutable and keyed by its inputs:

```text
representation ID =
    asset ID
  + representation recipe
  + recipe version
  + model/version where applicable
```

For example:

```text
asset_123
proxy:h264-720p-v3
```

If the laptop and server independently produce the same representation recipe, they should be considered equivalent.

Do not synchronize “proxy generation state” as a mutable object. Synchronize the fact that an immutable representation now exists at a location.

## 4. Media blobs: explicitly transferred

Originals and proxies should not behave like ordinary project state.

Originals:

* Stay where they are unless the user requests archival or copying
* Are represented through location manifests
* Are never silently duplicated

Proxies:

* Can be generated on either node
* Can be downloaded or deleted independently
* Can be pinned for offline use
* Can be regenerated from an available original

Contact sheets and small thumbnails can generally synchronize automatically.

## 5. Ephemeral UI state: never synchronized

Do not sync:

* Window size
* Expanded panels
* Current playhead position
* Hover state
* Temporary search query
* Local job-console visibility

You may sync deliberate workflow state such as “last reviewed clip,” but not every UI interaction.

# Recommended sync protocol

Because this is one user with limited concurrency, I would build a small domain-operation protocol rather than immediately adopting a general CRDT.

Each device stores:

```text
device ID
next local sequence number
highest sequence received from every other device
```

When reconnecting:

```text
Mac → Server:
I have:
  macbook through op 4182
  server through op 9501

Server → Mac:
Send me macbook 4183+
Here are server 9502+
```

The connection can use:

* HTTP for initial reconciliation
* WebSocket for live operation delivery
* Periodic reconciliation as a safety net

Every operation should have:

* Globally unique operation ID
* Origin device ID
* Monotonic sequence for that device
* Hybrid logical timestamp
* Entity or aggregate ID
* Operation type
* Payload
* Optional base revision

The hybrid logical timestamp avoids relying solely on the Mac and server having identical wall clocks.

# Conflict rules

Your “single person, latest change usually wins” assumption allows relatively simple behavior, but not every domain should use the same rule.

| Data                     | Resolution                                 |
| ------------------------ | ------------------------------------------ |
| Simple scalar fields     | Last logical write wins                    |
| Labels                   | Add/remove operations merge                |
| Notes                    | Preserve revisions; latest becomes active  |
| Script                   | Immutable revisions; one marked active     |
| Edit plan                | Immutable revisions; never overwrite       |
| Asset locations          | Merge all observations                     |
| Analysis results         | Merge by recipe/model identity             |
| Trip phase name          | Last logical write wins                    |
| Trip phase boundaries    | Last write wins, preserve prior revision   |
| Clock epoch adjustment   | Domain revision with stale-write detection |
| Contradictory chronology | Surface conflict; never silently LWW       |
| Job results              | Immutable; duplicates can be deduplicated  |

## Example: notes changed on both surfaces

The laptop writes:

```text
Revision 8: “Breakfast before leaving the hotel.”
```

The web app writes:

```text
Revision 9: “Breakfast buffet before leaving for the station.”
```

Both survive. Revision 9 may become active, but the UI can show that another revision exists.

## Example: chronology changed concurrently

The laptop sets:

```text
Sony C0163 onward: −8 hours
```

The browser sets:

```text
Sony C0148 onward: −7 hours
```

Do not choose one only because its wall-clock timestamp is later.

Show:

```text
Chronology calibration changed on two surfaces.

Laptop:
C0163 onward · −8h

Home web:
C0148 onward · −7h

[Use laptop]
[Use home web]
[Inspect timeline]
```

Chronology operations affect too much downstream state to silently discard one.

# Why not use SQLite changesets as the sync system?

SQLite’s Session extension can record table changes as changesets and apply them to another compatible database, including conflict callbacks. ([SQLite][3])

It is tempting, but I would not use it as the product-level sync protocol because:

* Sync becomes tightly coupled to database schema
* Conflicts occur at row level rather than product-domain level
* A row conflict cannot naturally explain “two competing clock epochs”
* Schema migrations complicate old changesets
* Agent-visible revision history becomes harder to express
* Derived/materialized tables might accidentally become synchronized

It may be useful internally for:

* Undo
* Testing
* Database snapshots
* Migration support

But domain operations are the better synchronization contract.

# When Automerge would make sense

Automerge becomes attractive if you later want:

* Offline web editing
* Several peer devices without a reliable hub
* Fine-grained concurrent Markdown or rich-text editing
* Automatic merge behavior across many clients
* Branching project experiments

Its sync protocol is transport-agnostic and transfers only changes needed by the other peer; repositories can store documents locally and synchronize them over WebSockets. ([Automerge][4])

For the current product, a full CRDT data model is probably more complexity than benefit. You already have important domain-specific conflict behavior that must be understood by the application.

A later hybrid is possible:

```text
Domain operation log:
chronology, assets, jobs, revisions

Automerge document:
live collaborative script or notes
```

# Processing and job division

Every job should declare:

```json
{
  "job_type": "generate_proxy",
  "asset_id": "asset_123",
  "requires": {
    "original": true
  },
  "preference": "background",
  "recipe": "h264-720p-v3"
}
```

The scheduler can then route it.

## Prefer laptop when

* The job is interactive
* The original exists only on an attached SD card
* The user needs the result immediately
* It is part of the current review loop
* The server is unavailable

## Prefer server when

* The original is on the NAS
* The job is a large batch
* The job can run overnight
* It involves transcription or VLM processing
* The result should be shared across future projects
* The laptop should not remain awake

## Avoid distributed-job complexity initially

For MVP, do not create a global competitive worker pool.

Use explicit routing:

```text
Run here
Run on home server
Auto
```

Under `Auto`:

1. Run interactive jobs locally.
2. Run NAS-backed batch jobs on the server.
3. If only one node has the original, run there.
4. If the preferred node is offline, mark the job pending rather than silently moving huge originals.

Later, nodes can claim jobs using leases.

# Proxy workflow

## Media discovered on the NAS

```text
Server scans original
        ↓
Registers asset and NAS location
        ↓
Creates contact sheet and review proxy
        ↓
Syncs metadata and thumbnail manifest to Mac
        ↓
Mac downloads proxy only when requested or pinned
```

## Media discovered on an SD card attached to the Mac

```text
Mac registers asset
        ↓
Creates immediate contact sheet / proxy
        ↓
Syncs project metadata to server
        ↓
User optionally selects “Archive to home”
        ↓
Original transfers and is verified
        ↓
Server registers NAS location
```

## Project complete

```text
Mac deletes review proxies
        ↓
Project metadata and contact sheets remain
        ↓
Server retains or regenerates proxies according to policy
```

# Web and desktop frontend sharing

Use the same React component packages for both surfaces:

```text
packages/
├── review-ui/
├── timeline-ui/
├── trip-context-ui/
├── asset-browser-ui/
├── project-ui/
└── shared-types/

apps/
├── desktop-react/
└── server-web/
```

Provide a common frontend service interface:

```ts
interface ProjectClient {
  getProject(id: ProjectId): Promise<Project>;
  appendOperation(op: ProjectOperation): Promise<void>;
  searchClips(query: ClipSearch): Promise<ClipResult[]>;
  requestRepresentation(
    assetId: AssetId,
    recipe: RepresentationRecipe,
  ): Promise<Job>;
}
```

Desktop implementation:

```text
TauriProjectClient
    → Tauri command
    → local Rust core
```

Web implementation:

```text
HttpProjectClient
    → server HTTP/WebSocket
    → server Rust core
```

The React components should not care which adapter they are using.

# Rust process layout

```text
Rust crates
├── rcd-domain
├── rcd-catalog
├── rcd-project
├── rcd-chronology
├── rcd-sync
├── rcd-jobs
├── rcd-media
├── rcd-api
└── rcd-agent-api

Binaries
├── rcd-desktop        Tauri
├── rcd-server         HTTP, WebSocket, background jobs
├── rcd-cli
└── rcd-mcp
```

The server can use the same domain and persistence crates as the desktop, but a separate local database.

# Conceptual UX

The product should feel like **one project appearing on multiple surfaces**, not like a desktop program plus an unrelated server admin panel.

## Global status

Every surface should have a small status indicator:

```text
Home node: Online
Project: Synced
3 server jobs running
Laptop has 2 unsynced edits
```

Avoid turning ordinary sync into a prominent workflow. Make it visible mainly when attention is required.

## Asset availability

Use capability-oriented badges:

```text
Original       Home node
Review proxy   Laptop + Home node
Contact sheet  Laptop + Home node
Current cut    Ready offline
```

Do not primarily show paths unless the user opens details.

## Actions should communicate execution location

```text
Generate review proxies
Auto · Home node recommended

Render quick preview
Laptop · interactive

Transcribe full archive
Home node · background
```

## Project opening

The same project page can say:

```text
Available here:
✓ Notes and chronology
✓ Contact sheets
✓ 214 review proxies
△ 19 proxies available only from home
✗ Originals unavailable while offline
```

## “Prepare offline”

This becomes a central bridge between server and laptop:

```text
Prepare project on this Mac

✓ Project state
✓ Contact sheets
✓ Review proxies
□ Original media used by current cut
□ All original media

Required space: 18.4 GB
```

## “Send home”

For travel media:

```text
Archive this project to home node

32 originals not yet archived
84 GB total

After verification:
□ Keep SSD copy
□ Allow local cleanup
```

## Web capability restrictions

When a browser lacks the required representation, do not show a generic error.

Show:

```text
This original is available only on the travel SSD.

You can:
• View its contact sheet
• Add notes
• Ask the laptop to create a proxy when it reconnects
```

# Remote access

For a personal home-server deployment, expose the web server privately over Tailscale rather than directly opening it to the public internet. Tailscale Serve can proxy a local web application to other devices within the same tailnet; Funnel is the separate feature for making a service public and is not needed here. ([Tailscale][5])

A reasonable server setup is:

```text
rcd-server listens on localhost:8080
        ↓
Tailscale Serve provides HTTPS
        ↓
Only authenticated tailnet devices can access it
```

You may later add normal application authentication, but Tailscale identity is a good personal-deployment boundary.

# Recommended MVP sequence

## Phase 1: server without editing

* Server scans NAS
* Generates proxies
* Hosts project and asset API
* Browser browses assets and monitors jobs
* Laptop manually connects to server

## Phase 2: metadata synchronization

* Domain operation log
* Laptop/server reconciliation
* Notes and labels synchronize
* Availability manifests synchronize
* Derived representation records synchronize

## Phase 3: web review

* Browser proxy playback
* Notes and labels
* Review status
* Script and edit-plan inspection

## Phase 4: web chronology

* Shared timeline calibration UI
* Server writes chronology operations
* Laptop receives and materializes them
* Conflict UI for overlapping clock-epoch changes

## Phase 5: offline preparation and archival

* Proxy download
* Representation pinning
* Send originals home
* Checksum verification
* Safe-local-cleanup workflow

# The key product distinction

The server should not be thought of as “the backend.”

It is another RCD VlogCutter node with different capabilities:

```text
Laptop node
Interactive · portable · sometimes offline

Home node
Archive-connected · always on · high-capacity

Web browser
Thin window into the home node
```

That mental model preserves local-first behavior while still giving the server a clear and valuable role.

[1]: https://automerge.org/docs/hello/?utm_source=chatgpt.com "Welcome to Automerge"
[2]: https://www.sqlite.org/fileformat.html?utm_source=chatgpt.com "Database File Format"
[3]: https://www.sqlite.org/sessionintro.html?utm_source=chatgpt.com "The Session Extension"
[4]: https://automerge.org/docs/reference/concepts/?utm_source=chatgpt.com "Concepts"
[5]: https://tailscale.com/docs/features/tailscale-funnel?utm_source=chatgpt.com "Tailscale Funnel · Tailscale Docs"

Q: the laptop and server can both access the NAS independently (and the path will be different).  Does this plan take that into account? How can we take this into account.

**The plan mostly supports it, but the storage model should be made more explicit.** We should not represent these as two separate original locations:

```text
Mac:    /Volumes/TravelNAS/Argentina/C0103.MP4
Server: /mnt/nas/travel/Argentina/C0103.MP4
```

They are two node-specific ways of reaching the **same physical file on the same logical storage root**.

# Revised model: asset → storage root → node binding

Use three distinct concepts.

## 1. Logical asset

The video itself, identified independently of any path:

```json
{
  "asset_id": "asset_7f31e2",
  "fingerprint": "abc123...",
  "filename": "C0103.MP4"
}
```

## 2. Logical storage location

Where that instance of the asset resides conceptually:

```json
{
  "storage_root_id": "nas-main-video",
  "relative_path": "Argentina/Day 03/Sony/C0103.MP4"
}
```

The canonical location is:

```text
nas-main-video + Argentina/Day 03/Sony/C0103.MP4
```

It is **not** an absolute macOS or Linux path.

## 3. Node-specific mount binding

Each node independently maps that logical root to a path it can access:

```json
{
  "node_id": "ryan-macbook",
  "storage_root_id": "nas-main-video",
  "local_path": "/Volumes/TravelNAS",
  "access": "read_write"
}
```

```json
{
  "node_id": "home-server",
  "storage_root_id": "nas-main-video",
  "local_path": "/mnt/nas/travel",
  "access": "read_write"
}
```

The nodes resolve the asset locally:

```text
Mac:
/Volumes/TravelNAS
+ Argentina/Day 03/Sony/C0103.MP4

Server:
/mnt/nas/travel
+ Argentina/Day 03/Sony/C0103.MP4
```

This is the key addition.

# Conceptual hierarchy

```text
Logical asset
asset_7f31e2
    │
    ▼
Physical storage object
nas-main-video / Argentina/Day 03/Sony/C0103.MP4
    │
    ├── Mac binding
    │   /Volumes/TravelNAS/Argentina/Day 03/Sony/C0103.MP4
    │
    └── Server binding
        /mnt/nas/travel/Argentina/Day 03/Sony/C0103.MP4
```

This avoids incorrectly treating the file as two copies.

By contrast, a real copy on a USB SSD would be another physical location:

```text
asset_7f31e2
├── NAS location
│   nas-main-video / Argentina/.../C0103.MP4
│
└── USB location
    travel-ssd-2026 / Working/.../C0103.MP4
```

# What should synchronize

Synchronize:

* Logical storage-root ID
* Root display name
* Asset fingerprint
* Relative path
* File size
* Storage-object identity
* Which nodes currently claim access
* Read/write capabilities
* Last successfully verified time

Do **not** synchronize an absolute path as the canonical path.

Absolute paths belong to each node’s local configuration:

```text
Mac configuration:
nas-main-video → /Volumes/TravelNAS

Server configuration:
nas-main-video → /mnt/nas/travel
```

A node may share the fact that it has a binding, but the path itself should not be used by other nodes.

# Setup UX

When configuring storage on the Mac:

```text
Add storage

Selected folder:
/Volumes/TravelNAS

What is this?

● Connect to existing storage root
  Main NAS Video

○ Register as a new storage root
```

On the server:

```text
Add storage

Selected folder:
/mnt/nas/travel

Possible match:
Main NAS Video

Evidence:
✓ 178 sampled assets match
✓ Directory structure matches
✓ File fingerprints match

[Link as the same storage]
```

Afterward:

```text
Main NAS Video

MacBook
/Volumes/TravelNAS
Available · read/write

Home server
/mnt/nas/travel
Available · read/write
```

# How to recognize that two paths are the same NAS root

There are several possible signals.

## Best case: persistent root marker

If you have a writable management directory, RCD VlogCutter can create something like:

```text
.rcdvlog-storage-root
```

containing:

```json
{
  "storage_root_id": "nas-main-video"
}
```

Both nodes read the same identifier despite using different mount paths.

However, this must not be required because some archive roots may be read-only.

## Read-only fallback

When a marker cannot be written, match roots using:

* User confirmation
* Matching relative directory structure
* Matching fingerprints of sampled files
* Matching file sizes
* NAS share information when available
* Volume/share identifiers when reliable
* A saved association in each node’s local configuration

The application might say:

```text
These folders appear to be the same storage root.

Mac:
  /Volumes/TravelNAS

Server:
  /mnt/nas/travel

Evidence:
  50/50 sampled assets match

[Link them]
[Keep separate]
```

Manual confirmation should be available because network-share metadata is not always stable across SMB and NFS.

# SMB versus NFS does not matter

The server could use:

```text
/mnt/nas/travel
```

through NFS, while the Mac uses:

```text
/Volumes/TravelNAS
```

through SMB.

They can still bind to the same logical root.

A root binding can store details such as:

```json
{
  "storage_root_id": "nas-main-video",
  "node_id": "home-server",
  "protocol": "nfs",
  "mount_path": "/mnt/nas/travel",
  "access": "read_write"
}
```

```json
{
  "storage_root_id": "nas-main-video",
  "node_id": "ryan-macbook",
  "protocol": "smb",
  "mount_path": "/Volumes/TravelNAS",
  "access": "read_only"
}
```

Permissions may differ even though the underlying files are the same.

# Node availability

Availability is node-relative.

For the same asset:

```text
Original on Main NAS

This Mac:
✓ Available

Home server:
✓ Available

Travel laptop while away:
✗ NAS not mounted
```

The catalog should distinguish:

* Known to exist
* Accessible from this node
* Accessible from another online node
* Temporarily unavailable
* Missing or fingerprint mismatch

The browser, because it runs against the server, inherits the server’s media access.

# Proxy handling becomes better with this model

Since both nodes can read the NAS originals, they do not need to transfer originals between each other simply to generate proxies.

## Interactive job

The Mac needs a proxy now:

```text
Mac reads:
/Volumes/TravelNAS/Argentina/.../C0103.MP4

Mac generates:
local proxy cache
```

## Background job

The server processes the same asset overnight:

```text
Server reads:
/mnt/nas/travel/Argentina/.../C0103.MP4

Server generates:
server or NAS proxy cache
```

Both jobs refer to the same logical asset and same original storage object.

# Recommended proxy-storage model

Use two levels of derived storage.

## Shared derived store

A dedicated NAS directory:

```text
nas-derived/
├── proxies/
├── contact-sheets/
├── transcripts/
└── analysis/
```

The server generally writes here.

Both nodes have different bindings:

```text
Mac:
/Volumes/RCDDerived

Server:
/mnt/nas/rcd-derived
```

## Local hot cache

The Mac also keeps a local cache for:

* Currently active project proxies
* Offline travel
* Faster scrubbing
* Recently accessed media

Conceptually:

```text
Representation: asset_7f31e2 / proxy:h264-720p-v3

Locations:
├── Shared derived NAS
└── Mac local cache
```

The same proxy recipe should not be generated twice unnecessarily.

A representation job should be keyed by:

```text
asset ID
+ original fingerprint
+ proxy recipe
+ recipe version
```

# Job routing when both nodes can access the original

The scheduler should know:

```text
Asset original available from:
✓ Mac
✓ Server

Requested output:
Review proxy
```

Then use a policy:

## Interactive request

```text
User pressed Play and proxy is missing
→ Run on Mac
```

## Background batch

```text
Generate proxies for 400 NAS clips
→ Run on server
```

## Already running elsewhere

```text
Server is already generating this proxy
→ Mac can wait, stream the result, or generate locally if urgent
```

The first implementation can keep this simple:

```text
Run:
● Auto
○ This Mac
○ Home server
```

Under `Auto`:

1. Interactive jobs prefer the current node.
2. Large NAS batches prefer the server.
3. Existing compatible output is reused.
4. Existing in-progress work is normally not duplicated.

# Shared files moved on the NAS

Suppose the file moves from:

```text
Argentina/Day 03/Sony/C0103.MP4
```

to:

```text
Archive/Argentina/Sony/C0103.MP4
```

Because both nodes refer to the same logical storage root, this should be treated as **one location update**, not two independent relinks.

Whichever node detects it first can report:

```json
{
  "asset_id": "asset_7f31e2",
  "storage_root_id": "nas-main-video",
  "old_relative_path": "Argentina/Day 03/Sony/C0103.MP4",
  "new_relative_path": "Archive/Argentina/Sony/C0103.MP4",
  "verified_by_fingerprint": true
}
```

After synchronization, both resolve the new path using their own mount bindings.

```text
Mac:
/Volumes/TravelNAS/Archive/Argentina/Sony/C0103.MP4

Server:
/mnt/nas/travel/Archive/Argentina/Sony/C0103.MP4
```

This is one of the main benefits of making root-relative paths canonical.

# Partial mounts

The Mac might mount only a subdirectory rather than the entire NAS root.

For example:

```text
Logical root:
nas-main-video

Mac mount:
/Volumes/Argentina

Corresponds to root subpath:
Trips/2025/Argentina
```

The binding should support a prefix:

```json
{
  "node_id": "ryan-macbook",
  "storage_root_id": "nas-main-video",
  "local_path": "/Volumes/Argentina",
  "root_subpath": "Trips/2025/Argentina"
}
```

That node can access only assets beneath that subtree.

# Revised role model

I would slightly adjust the previous description:

## Laptop

* Primary interactive client
* Peer media-access node
* Can independently read the NAS
* Can generate local proxies directly from NAS
* Can work offline using local cached media

## Server

* Sync hub
* Peer media-access node
* Background processor
* Web host
* Usually preferred for bulk NAS work
* Not a mandatory gateway between the Mac and NAS

## NAS

* Shared logical storage
* Independently mounted by both nodes
* May store originals and shared derived files
* Does not contain the live SQLite project database

```text
                 Shared logical NAS root
                 nas-main-video
                    ▲          ▲
different path     │          │     different path
                    │          │
             Mac desktop    Home server
                  ↕              ↕
             local database  local database
                    ↔ sync operations
```

# Data model I would use

```text
Asset
  asset_id
  fingerprint

StorageRoot
  storage_root_id
  display_name
  kind
  shared

StorageObject
  storage_object_id
  storage_root_id
  relative_path
  fingerprint
  size

AssetLocation
  asset_id
  storage_object_id

NodeRootBinding
  node_id
  storage_root_id
  local_mount_path
  root_subpath
  access_mode
  protocol
  last_verified_at

Representation
  representation_id
  asset_id
  recipe
  recipe_version

RepresentationLocation
  representation_id
  storage_object_id
  node_local_cache_id
```

`NodeRootBinding` is the critical addition.

# UX principle

Do not normally show users both absolute paths.

Show:

```text
Original
Main NAS Video
Available on this Mac and Home Server
```

Only show node-specific paths in a details or diagnostics panel:

```text
This Mac:
/Volumes/TravelNAS/Argentina/...

Home server:
/mnt/nas/travel/Argentina/...
```

That keeps the product mental model centered on storage identity rather than mount implementation.

So the revised answer is:

> Both nodes should treat the NAS as one shared logical storage root, while maintaining independent local mount bindings. Asset synchronization uses root-relative paths and fingerprints; absolute paths remain node-local.
