# Card Import Architecture

**Status:** Implemented; awaiting the physical-card/physical-volume acceptance gate
**Date:** 2026-08-09
**Scope:** Desktop SD-card import, physical location identity, and relink
**Depends on:** [Desktop Architecture](../desktop/ARCHITECTURE.md)
**Related:** [Travel Ingest Architecture](../travel/ARCHITECTURE.md) — shares the card-identity
formula and the create-only commit discipline; neither epic blocks the other

## 1. Executive summary

Today "import" in VlogCutter means *add a reference to this file wherever it currently lives*.
`ingest_file` stores the source path on the asset, proxies go to the project cache, and nothing
copies anything. That is coherent for a permanent NAS folder and a poor reading of "ingest this
camera card," because the card is the one storage location guaranteed to disappear.

This epic makes card import a first-class workflow and — more importantly — makes an asset's
**physical location** a modeled, verified, repairable fact rather than a bare path string.

Three changes, in dependency order:

1. **Location identity.** An asset's location gains verification state, and "is the original
   available" stops meaning "does a file exist at that path." A known file discovered at a new
   path repairs the asset instead of being discarded as a duplicate.
2. **Archive roots.** A logical writable root plus a relative path, so the destination an import
   lands in can itself be remounted or moved without stranding anything.
3. **Preflight and the import modal.** Classify a whole card before mutating anything, then show
   the user the delta *and the destination* in one dialog they can act on with one click.

## 2. Accepted product decisions

Confirmed with the user on 2026-08-09. These take precedence over older statements.

1. **The app copies from removable cards only.** SD card → anywhere is an import, and the copy is
   the application's job. Everything downstream — local → external drive → NAS — is the user
   moving their own files, and the application's only responsibility there is to notice the media
   moved and **ask where the originals are now**. App-managed lifecycle migration is future work
   and explicitly out of scope (§10).
2. **A non-removable source is still referenced in place.** Ingesting a NAS or local folder behaves
   exactly as it does today. No bytes are copied.
3. **The delta is scoped to the open project.** "18 new, 225 already known" means known *to this
   project*. Cross-project and library-wide recognition are out of scope (§10).
4. **The import modal must state where originals are going**, in the same glance as the delta. A
   copy destination that the user has to infer is a defect, not a polish item.
5. **Card contents are never modified.** No formatting, no deletion, no "safe to eject" claim, no
   writes of any kind to a mounted card. This matches the travel epic's §3 durability bar.

### 2.1 Amendment to the referenced-in-place invariant

`AGENTS.md` §2 states: *"Originals are immutable and referenced in place. Never move, rename,
rewrite, or delete source media."* PRD §25.1.5 states originals remain on NAS or removable storage.

Both are amended, narrowly and with user approval:

> Media discovered on a **removable volume** may be **copied** into a configured writable archive
> root as part of import. The source card is read-only throughout and is never modified. Media on
> any non-removable source continues to be referenced in place.

What is *not* amended: nothing moves, renames, rewrites, or deletes source media, on a card or
anywhere else. The archive root is create-only (§4.3). Existing NAS scan roots keep their
`immutable_original` status.

## 3. Location identity

This is the foundation and the reason the epic is sequenced the way it is.

### 3.1 The model that already exists

`project_asset_local_locations(asset_id, node_id, local_path, created_at)` already exists in
`core/rcdvlog-core/src/migrations.rs`, is written on every ingest by `add_project_asset`, and is
backfilled by an existing migration. Project operations are deliberately path-free
(`operations.rs` — "`assets.path` and `assets.proxy_path` are node-local absolutes and are
deliberately absent").

So `assets.path` is **a denormalized cache of the current node's location**, not the model of
record. This epic extends a table that exists; it does not introduce a competing model.

**Consequence for implementers:** a relink is a node-local table write. It is *not* a replicated
project operation. Paths are node-local by deliberate design and pushing a Mac mount path into the
server replica would be a protocol violation.

### 3.2 What is broken

Two defects share one root cause.

`row_to_asset` computes `original_available = Path::new(&path).is_file()` — existence only. The
stored fingerprint is never re-checked after ingest.

- **Missed repair.** A card reappears at a different mount point. `ingest_file` computes the
  fingerprint, finds it already present, returns "duplicate," and leaves the asset pointing at the
  old offline path. The system saw the missing original and failed to heal it.
- **False identity.** A card is reformatted and reused, and a new `C0012.MP4` lands at the same
  `/Volumes/CARD/DCIM/100MSDCF/C0012.MP4`. The old asset reports its original as available because
  a file exists at that pathname. Proxy generation and edit-plan export
  (`editplan/validate.rs`, existence-only) will then operate on the wrong bytes, silently.

The second is rarer — it needs the same volume label, DCIM path, filename, and a surviving asset —
but both are fixed by the same primitive, and the first is an everyday occurrence for anyone whose
NAS remounts at a new path.

### 3.3 The fix

Extend the location table:

```text
project_asset_local_locations(
    asset_id, node_id, local_path, created_at,
    verification_state,   -- unverified | provisional | verified | mismatch
    verified_fingerprint, -- the sampled fingerprint observed at verify time
    verified_at
)
```

The primary key relaxes from `(asset_id, node_id)` to allow more than one location per node, so a
card and its archive copy can both be known.

Rules:

- **Availability requires verification.** `original_available` becomes true only for a location
  whose file exists *and* whose sampled fingerprint matches the asset's. A stale `verified_at`
  triggers re-verification; it never grants availability on its own.
- **A mismatch is recorded, not overwritten.** A path whose bytes no longer match transitions to
  `mismatch` and stops being a usable location. It is never silently rebound.
- **A duplicate discovered at a new path repairs.** When ingest computes a fingerprint that already
  belongs to an asset, it registers the discovered path as a verified location for that asset
  rather than discarding the observation. This is the whole of PRD §25.2.3's deferred "explicit
  relinking," delivered as a side effect of import rather than as a separate mode.

`assets.path` continues to be maintained as the preferred verified location, so every existing
reader keeps working unchanged.

### 3.4 Which hash, where

The sampled fingerprint (size + first and last 1 MiB, `sampled_media_fingerprint`) is the right
tool for identity and for the relink check: it is cheap, and the failure it guards against is
"this is a different file," not bit rot.

It is **not** adequate to verify a copy. Card copies are verified with a full-file BLAKE3 over the
written bytes (§4.4). The two hashes have different jobs; do not let the sampled one do both. The
server already models this pair as `quick_fingerprint` / `full_content_hash` on `media_objects`.

### 3.5 Asking where the originals went

Per §2.1 the user relocates originals themselves after import. The application's obligation is to
notice and ask.

`detect_offline_volumes` already groups unreachable originals by volume root and records one
`media.volume_offline` diagnostic per root with an affected-asset count. The relink surface hangs
off that diagnostic — the grouping work is done.

The interaction is a **Locate…** action on the offline-volume diagnostic:

1. The user picks the folder where that volume's media now lives.
2. Preflight (§6) scans it and matches by fingerprint, not by filename or path.
3. Matches are registered as verified locations for their assets; the diagnostic resolves with a
   count of what was repaired and what is still missing.

This is the same preflight machinery the import modal uses, pointed at a different question. One
classifier, two entry points.

## 4. Archive roots

### 4.1 Why a logical root

If import copies a card to `/Volumes/Shuttle/VideoArchive/...` and stores that absolute path, the
epic has recreated the exact bug it set out to fix, at 500 GB scale — because an external archive
drive is itself a thing that remounts elsewhere. The destination must be **a logical root plus a
relative path**.

The server already has this model (`storage_roots` / `node_root_bindings` with `local_mount_path`).
The desktop does not, and the desktop must work with no server present (`AGENTS.md` §2). So the
model comes to `rcdvlog-core`, deliberately smaller than the server's:

```text
archive_roots(archive_root_id, display_name, created_at)
archive_root_bindings(archive_root_id, node_id, local_mount_path, verified_at)
```

An archive root is a name the user recognizes ("NAS Video", "Shuttle SSD"). Its binding is where
that root is mounted *on this node right now*. Remounting elsewhere updates the binding; nothing
else in the catalog changes, and every asset archived under it stays resolvable. That is what makes
remount and move first-class rather than a repair mode.

### 4.2 Layout

Following travel §4.2, which was designed for this exact problem:

```text
<archiveRoot>/<projectSlug>/<cardSessionSlug>/<cardRelativePath>
```

- `projectSlug` derives from the project name (`2026-08-spain`).
- `cardSessionSlug` is `<cardLabel>-<cardSessionShortId>`, so a card reinserted later resumes into
  the same directory while a reformatted card gets a new identity and a new directory. DCIM counter
  wraparound cannot collide across sessions.
- `cardRelativePath` preserves the camera's own folder structure (`DCIM/100MSDCF/C0012.MP4`),
  keeping camera-native sidecars and clip groupings intact.

### 4.3 Write discipline

The archive root is writable only along the create-new-file path. No code in this epic renames,
overwrites, truncates, or deletes anything beneath it.

Commit is temp-file-in-the-same-directory, `fsync`, then atomic rename. A partial file is never
visible under its final name, and a failed copy leaves only a temp path the application owns and
cleans up. An existing file at the target name is never overwritten — it is a classification input
(§6), not a collision to resolve by clobbering.

### 4.4 Verification and resume

Each copied file is verified by full-file BLAKE3 computed over the bytes as they are written, then
compared against a re-read of the committed file. A mismatch discards the temp file and leaves the
object retryable; it never commits.

Interrupting an import is expected — cards are large and laptops close. Because commit is atomic
per file, resume is per-file and needs no chunk ledger: a file either committed or did not. Re-running
an import re-classifies and copies only what is missing.

Verification roughly doubles read cost on the destination. It stays on by default, because an
unverified copy of footage that is about to be the only copy is not worth having, and it is
overridable per import for a user who knows what they are trading away.

#### The source going away is one of those interruptions

A bus-powered enclosure browning out mid-import is a normal field failure, not an exceptional one,
and the per-file commit discipline above already makes it survivable. What it needs beyond that is
a way back, so `run_import` treats it as a distinct ending rather than as N unreadable files:

- `import_source_present` re-asks the source's identity — the §5 session id, never the pathname —
  on the first per-item error. If the source is gone, the run stops with
  `ImportOutcome.source_disconnected` and records no failure for the interrupted file: it copied
  nothing, so it is remaining work rather than a file needing attention. Proxies are skipped for
  the same reason, so the reconnect is not buried behind minutes of unrelated work.
- The desktop keeps the session — destination, camera choice, verification and proxy preferences,
  and the card identity — and offers it from the media menu as well as from the dialog, because a
  modal the user closed must not be the only way back into live work.
- Mount detection (§8) recognizes that one card by session identity even when its banner was
  dismissed, which happens the moment the user chooses to review it. Dismissal is a statement about
  a banner, not about a session already under way.
- Resume is an ordinary re-run: preflight re-classifies, already-committed files come back as
  known, and only the remainder copies. A *different* card at the same mount path is never the
  interrupted source; nothing is inferred from a pathname.

## 5. Card identity

A card session is identified by the volume UUID or label combined with a digest of the first N
`(relativePath, size)` pairs in stable order — the formula from travel §5.3, lifted into
`rcdvlog-core` so the desktop does not wait on a nine-task mobile epic to use it. Travel T01-T002
consumes the same primitive later.

This gives the three properties the workflow needs:

- Reinserting the same card yields the same identity, so the import resumes.
- A reformatted card yields a different identity, so it starts clean and cannot collide with the
  old card's archive directory.
- Shooting more footage onto a partially imported card preserves identity: new files are added, and
  already-imported files stay classified as known.

Card sessions are persisted per project so the second insertion of a known card is genuinely a
one-click operation.

### 5.1 Recording which card a sequence came from

Ingest creates one capture sequence per camera, and until #74 it created them
with empty `volume_hint`/`filename_prefix`. An empty hint means "matches any
run", so every later import from the same camera model extended the same
sequence and the reformatted-card detectors — the ones §9 asks for — could
never fire outside their own tests.

A completed import now records the card it read, once, on the sequences its new
clips joined. Four properties make that safe:

- **Observed, not inferred.** It records where bytes came from, witnessed with
  the card in the slot. It changes no ordering, no time, and no membership, so
  it is not a chronology correction and import gains no confirmation gate. The
  only thing it can cause is the app asking *more* of a later import.
- **First write wins.** A sequence that already names a card keeps it.
  Overwriting would erase the very evidence that recognizes the next reformat.
- **Cards only.** A folder import has no durable identity to record; a mount
  path is not a name, and an empty hint keeps matching anything.
- **An operation, not an UPDATE.** It goes through `captureSequenceDeclared`,
  so it is logged, revision-guarded, and carried to peers like every other
  project mutation.

A camera whose clips in one run used two filename prefixes records no prefix,
only the card: picking one of two would manufacture a mismatch on the next
import out of this run's own ambiguity.

### 5.2 Detecting the camera

Camera identification for the initial *banner* comes from folder structure:
`DCIM/100MSDCF` (Sony), `DJI_*` (DJI), `100GOPRO` (GoPro). Smart ingest then performs one bounded
ExifTool read from the first sorted new clip so a user-corrected camera identity can be remembered
by project across remounts without conflating two bodies that share a brand and filename pattern.
If that representative clip has no readable make/model, the dialog keeps the choice explicit for
this run and does not create an ambiguous sticky mapping.

The existing filename-pattern table (`docs/tasks/desktop/CAMERA_PATTERNS.md`) is unchanged and
still authoritative for per-asset camera identity, behind EXIF. Folder structure is a card-level
hint for the banner and the archive path; it never overrides EXIF on an asset.

### 5.3 "Everything before this clip is old"

A card that has lived in a camera for months carries footage that predates the project, and
re-presenting it on every reinsertion is noise. The project stores one boundary per card session
(`import_cutoff`), and preflight applies it between classification and every number derived from
classification — so an excluded clip never reaches the byte cost, the camera evidence, or a
capture-sequence proposal.

The rule is keyed on the **card session**, deliberately not on a mount path (transient) and not on
camera evidence (shared by every card that camera ever wrote, so a brand-new card would arrive
already filtered — a silent filter over unseen footage). The boundary itself is the clip's
card-relative path, which is stable wherever the volume mounts and is exactly the order the file
ledger displays, so "everything above this row" is a rule the user reads rather than infers.

Three properties are load-bearing:

- **Excluded is not known, and not failed.** `excluded_by_cutoff` is reported separately from
  `known_clips`, because "you already have these" and "you told me to ignore these" lead to
  different actions. `PathCollision` and `Unreadable` before the boundary are absorbed too: both
  are only problems because the file was going to be imported.
- **A cutoff never suppresses a relink.** A clip before the boundary whose original this project
  has lost stays `KnownRelinkable`, is counted in `relinkable_before_cutoff`, and is marked as such
  in the ledger. A relink copies nothing, so there is no cost to weigh against leaving the
  project broken.
- **It is never invisible.** The boundary row is marked in the ledger, the banner above the list
  names the clip and the number of exclusions, and the dialog summary carries both a lead stat and
  a one-click route back to the boundary row.

## 6. Preflight

Preflight walks a source and returns a classification per candidate file **without mutating the
catalog**. It is the shared engine behind the import modal, the relink flow, and any future
automation.

| Classification | Meaning | Action offered |
| --- | --- | --- |
| `New` | Fingerprint not in this project | Copy (card) or reference (folder), then create the asset |
| `KnownVerified` | Fingerprint known, a verified location is online | Nothing |
| `KnownRelinkable` | Fingerprint known, no online location, found here | Register this as a verified location |
| `KnownMissingProxy` | Fingerprint known and online, proxy missing or stale | Generate proxy |
| `PathCollision` | Target archive path exists with different content | Report; never overwrite |
| `Unreadable` | Cannot be probed or hashed | Report; record a diagnostic |

`KnownRelinkable` is the classification that fixes §3.2. It is the difference between "duplicate,
skipped" and "found your missing footage."

Preflight reports progress (`scanned / total`, current file) and is cancellable. On a card of a few
hundred 4K clips the sampled fingerprint keeps classification to seconds. ffprobe remains deferred;
the only metadata process in the initial plan is the single representative ExifTool read above.

## 7. The import modal

One dialog, three facts, one button. The destination is not a detail line — it is a peer of the
delta, per §2.4.

```text
┌──────────────────────────────────────────────────────────────┐
│  DJI_SD detected — DJI Pocket 3                              │
│                                                              │
│  243 clips · 18 new · 225 already in Spain 2026              │
│  31 clips need proxies                                       │
│                                                              │
│  Originals →  NAS Video                                      │
│               /Volumes/nas/VideoArchive/2026-08-spain/       │
│               DJI_SD-4f2a/                                   │
│               copies 62.4 GB · 1.2 TB free      [Change…]    │
│                                                              │
│  Proxies   →  Spain 2026 project cache (this laptop)         │
│                                                              │
│  ☑ Verify copies    ☑ Make missing proxies                   │
│                                                              │
│              [ Cancel ]  [ Import 18 clips ]                 │
└──────────────────────────────────────────────────────────────┘
```

Requirements this encodes:

- **Destination is named and resolved.** The logical root name *and* where it currently resolves,
  because "NAS Video" alone does not tell the user whether it is mounted.
- **Byte cost and headroom before the button.** A multi-hundred-gigabyte copy never starts behind a
  single click without stating its size and the free space at the destination. Insufficient space
  disables import and says so.
- **No destination configured is a first-run state, not an error.** The modal prompts for an
  archive root inline on first card import.
- **A referenced-in-place import says so plainly** — where the originals will remain, and that they
  stay there until the user moves them.
- **Proxies are a remembered preference.** Analysis stays opt-in and out of this dialog; it carries
  cloud spend and belongs to the existing analysis surface.

Progress replaces the body in place: scan → classify → copy/verify → metadata → proxies, with
counts, throughput, and cancel. `ingest_folder_with_progress` already exists in core; the Tauri
command currently calls the non-progress wrapper, so this is plumbing rather than new domain work.

The post-run result is persistent, not a toast: every file that was new, relinked, skipped, failed,
or collided remains inspectable after the dialog closes.

## 8. Mount detection

Deliberately conservative. Tauri has no volume-mount event, so this is a low-frequency poll of
mounted volumes filtered to removable media. Detection surfaces **a banner, never an action**:

```text
DJI_SD detected — 243 clips        [ Review import ]   [ Dismiss ]
```

Nothing is hashed, probed, or copied until the user clicks. Auto-scanning a 500 GB card the instant
it appears is hostile, and the user's own framing ("stick an SD card in and maybe click an action")
already allows for the click. Dismissal is remembered for that card session.

Platform support is macOS first, matching the product's stated target. The detection seam is
trait-shaped so other platforms can be added without touching the classifier.

## 9. Testing strategy

- **Reformatted-card regression.** Ingest `C0012.MP4`, replace it with different content at the
  same path, and assert the asset does not report its original as available and never resolves to
  the new bytes. This is the §3.2 defect and it gets an explicit test before any automation lands.
- **Relink.** Ingest from one path, move the tree, re-ingest from the new path, assert the asset is
  repaired and no second asset is created.
- **Mismatch is terminal.** A location that fails verification stays `mismatch` and is not silently
  rebound by a later scan.
- **Archive remount.** Bind an archive root at one mount path, rebind it at another, assert every
  archived asset stays resolvable with no per-asset repair.
- **Create-only.** Adversarial `cardRelativePath` inputs cannot escape the archive root; a commit
  never overwrites an existing file; a failed commit leaves no file under the final name.
- **Interrupted import.** Kill mid-copy, re-run, assert no partial file committed and only the
  missing files transfer.
- **Source loss and resume.** Pull the source mid-copy: the run stops as an interruption rather
  than a cancellation, reports no failed files, keeps everything already committed, and a remount
  of the *same* card resumes only the remainder. A different card at that path does not.
  `apps/desktop/src/test/cuj/importRecovery.cuj.test.tsx` carries the same journey through the UI.
- **Card identity.** Same card resumes; reformatted card gets a new session and a new directory;
  added footage preserves identity.
- **Card provenance reaches the detectors.** A real import — no `declare_capture_sequence` in the
  setup — leaves its card on the sequence it created, so re-importing a reformatted card that
  reuses the same mount path produces `volumeChange` evidence and a run that needs review. The
  same card with more footage on it stays routine, and a folder import records nothing.
- **Import cutoff.** A boundary excludes only what precedes it, survives a remount at another
  path, does not reach a different card from the same camera, still finds footage shot after it,
  and never hides a lost original. `apps/desktop/src/test/cuj/importCutoff.cuj.test.tsx` carries
  set → close → restart → remembered → move → clear through the UI.
- **Preflight purity.** Preflight over a card leaves the catalog byte-identical.
- **Modal.** Destination, byte cost, and free space are asserted present; insufficient space blocks
  import. Desktop component tests under `apps/desktop/src/components/__tests__/`.

Tests use `tempfile` directories. No test points at `/Volumes` or `~/Movies/VideoProjects`.

## 10. Out of scope

- App-managed lifecycle migration (local → external → NAS). The user moves originals; the app asks
  where they went (§2.1). Explicitly deferred by user decision.
- Cross-project or library-wide delta. The delta is the open project (§2.3).
- Card formatting, deletion, or any write to a mounted card (§2.5).
- Chunked or resumable *network* transfer. That is the travel epic; this epic copies over a local
  filesystem where per-file atomic commit is sufficient (§4.4).
- Automatic analysis on import. Analysis stays opt-in on its existing surface (§7).
- Automatic proxy cache eviction. MVP cleanup remains explicit (PRD §25.1.5).
- Windows and Linux mount detection (§8).

## 11. Risks

| Risk | Mitigation |
| --- | --- |
| Verification doubles import time on large cards | On by default with a stated cost, overridable per import; per-file atomic commit means an interrupted verify costs one file |
| Archive root unmounted at import time | Binding resolution is checked in preflight; the modal reports the root as unavailable rather than failing mid-copy |
| A user expects the card to be safe to format | No surface ever claims it; the modal states originals were copied, not moved |
| Sampled fingerprint collision across distinct clips | Size is part of the digest and camera media is high-entropy; full BLAKE3 on the copy path catches a genuine collision at archive time |
| Location table growth | One row per asset per node per location; a few thousand assets is negligible |
| Mount polling costs battery | Low-frequency, listing-only, and stops when no project is open |

## 12. Definition of done

Card import is complete when the user can insert a card, click one action, see what is new and
where it is going before committing, and get verified copies in a named archive root with proxies
queued — and when moving that archive to another drive afterward costs one **Locate…** and no
per-asset repair.

A demo that imports a handful of files from a local folder does not constitute completion.
