# I01: Card Import

<!-- bounded-epic-metadata
{
  "schema_version": 1,
  "epic_id": "I01",
  "title": "Card Import",
  "base_branch": "main",
  "task_branch_template": "codex/{task_id_lower}-{slug}",
  "tasks_per_cycle": 1,
  "merge_requires_user_authorization": true
}
-->

## Outcome

Turn desktop import from "pick a folder and wait" into a card workflow: insert an
SD card, click one action, see what is new and **where it is going**, and get
verified copies into a named archive root with proxies queued.

Underneath that, make an asset's physical location a verified, repairable fact.
An original that moves — because a card remounted, or because the user relocated
their archive from a local disk to an external drive to a NAS — costs one
**Locate…** and no per-asset repair.

The epic succeeds when one real card, inserted twice with footage added between
insertions, imports incrementally without re-copying a byte, and when the archive
it landed in can be moved to another drive with every asset still resolvable.

## Why this epic exists

The core ingest primitives are already idempotent and worth keeping: recursive
scan, sampled-fingerprint dedupe before expensive probing, and a proxy batch that
generates only what is missing or stale. "Some clips imported, some not; some
proxies made, some not" is already handled correctly underneath.

The UI exposes those primitives as unrelated manual operations — a single-folder
picker, a generic busy state, and a separate proxy button — and beneath them the
physical-location model is a bare path string with no verification and no repair
path. `AGENTS.md` lists "no ingest preview" and "no relink workflow" as known
gaps; PRD §25.2.3 defers relinking explicitly.

This epic adds a card-import layer above the existing primitives and strengthens
location identity below them. It does not rewrite ingest.

## Frozen context

- Product contract: `docs/tasks/import/ARCHITECTURE.md` §2 (accepted decisions),
  PRD §25.1.5, §25.2.3
- Architecture contract: `docs/tasks/import/ARCHITECTURE.md`
- Repository conventions: `AGENTS.md` §2, §8, §12, §14, §15, §16
- Camera identity contract: `docs/tasks/desktop/CAMERA_PATTERNS.md`
- Shared primitives: `docs/tasks/travel/ARCHITECTURE.md` §4.2 (archive layout),
  §5.3 (card identity)

### Architectural constraints

- The app copies from **removable cards only**. Non-removable sources are
  referenced in place, exactly as today.
- Mounted cards are read-only. No task writes, renames, or deletes anything on a
  card, and no surface claims a card is safe to format.
- The archive root is create-only: temp file, fsync, atomic rename. No task adds
  an API that renames, overwrites, truncates, or deletes beneath it.
- Existing NAS scan roots keep `immutable_original` and are untouched.
- A location is node-local. Relink is a local table write, never a replicated
  project operation — project operations stay path-free.
- Availability requires fingerprint verification. Path existence alone never
  grants it.
- A location that fails verification becomes `mismatch` and is never silently
  rebound.
- The delta is scoped to the open project.
- Preflight never mutates the catalog.
- The sampled fingerprint is for identity; full BLAKE3 is for copy verification.
  Neither substitutes for the other.
- The desktop works with no server present. Archive roots live in
  `rcdvlog-core`, not behind a server dependency.

### Invariant amendment carried by this epic

`AGENTS.md` §2 and PRD §25.1.5 are amended narrowly, with user approval recorded
in `ARCHITECTURE.md` §2.1: media on a **removable volume** may be copied into a
configured writable archive root during import. Nothing moves, renames, rewrites,
or deletes source media. I01-T009 reconciles both documents.

## Queue

| Order | Task | Title | Depends on | Status |
| ---: | --- | --- | --- | --- |
| 10 | I01-T001 | Verified location identity and relink | — | complete |
| 20 | I01-T002 | Offline-volume Locate… repair flow | I01-T001 | complete |
| 30 | I01-T003 | Desktop archive roots and bindings | — | complete |
| 40 | I01-T004 | Removable volume detection and card session identity | — | ready |
| 50 | I01-T005 | Ingest preflight and classification | I01-T001, I01-T004 | ready |
| 60 | I01-T006 | Verified create-only card copy | I01-T003, I01-T005 | ready |
| 70 | I01-T007 | Import modal and one-click smart ingest | I01-T005, I01-T006 | ready |
| 80 | I01-T008 | Card-mount banner | I01-T004, I01-T007 | ready |
| 90 | I01-T009 | Import acceptance and documentation reconciliation | I01-T008 | ready |

### Sequencing rationale

**T001 is first and is not negotiable.** It is the correctness fix — availability
without verification is how an asset silently resolves to the wrong bytes after a
card is reformatted and reused — and every later task depends on locations being
trustworthy. Automating card import on top of an unverified location model would
industrialize the defect.

**T002 ships user-visible value immediately.** `detect_offline_volumes` already
groups unreachable originals by volume root; hanging a **Locate…** action off
that diagnostic delivers the remount/move story the user asked for, before any
card machinery exists.

**T003 precedes T006 deliberately.** Copying originals to an absolute path would
recreate the offline bug at archive scale. The logical-root model has to exist
before anything is written into it.

**T007 and T008 are last.** The modal is only honest once preflight can classify
and the copy path can report cost; mount detection is only useful once there is a
modal to open.

## Promotion rule

The epic is promoted only when every required implementation, QA, review, and
release-gate packet is complete. One task cycle never promotes unrelated work.

This epic is complete only when all nine task packets satisfy their acceptance
criteria and I01-T009 records evidence from a real card — inserted twice, with
footage added between insertions — and a real archive relocation between two
physical volumes. A folder import of synthetic fixtures does not constitute
completion.

## Cross-cutting verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm --dir apps/desktop test
pnpm --dir apps/desktop build
```
