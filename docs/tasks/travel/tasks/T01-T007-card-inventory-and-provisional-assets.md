# T01-T007: Card Inventory and Provisional Assets

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "T01-T007",
  "epic_id": "T01",
  "title": "Card Inventory and Provisional Assets",
  "slug": "card-inventory-and-provisional-assets",
  "status": "ready",
  "task_class": "implementation",
  "depends_on": [
    "T01-T002",
    "T01-T005",
    "T01-T006"
  ],
  "principal_invariant": "Three minutes after inserting a card, every clip is visible as an annotatable project asset on phone, web, and desktop — and notes written against those assets survive intact when media later arrives, because projectAssetId is allocated at inventory time and never changes.",
  "central_falsification": "Insert a real SD card with real camera media, inventory it, write notes against multiple clips before any upload, complete uploads, and assert every note is byte-identical and attached to the correct clip after linking. Then re-inventory after shooting more footage and assert new clips appear alongside existing ones with progress preserved.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Card identity → transfer session (T01-T002)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "implementer": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "independent_tester": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/travel/tasks/T01-T007-card-inventory-and-provisional-assets.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/**",
      "core/rcdvlog-sync/src/**",
      "core/rcdvlog-server/src/transfer.rs",
      "apps/desktop/src-tauri/**",
      "apps/mobile/src/**"
    ],
    "independent_tester": [
      "apps/mobile/__tests__/**"
    ],
    "integrator": [
      "apps/desktop/src/**",
      "apps/web/**"
    ]
  },
  "forbidden_paths": [
    "deploy/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-core",
    "cargo test -p rcdvlog-sync",
    "pnpm --dir apps/mobile typecheck",
    "pnpm --dir apps/desktop build",
    "pnpm --dir apps/web build",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/travel/EPIC.md",
      "regex": "\\| 70 \\| \\[T01-T007\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Let the phone inventory an attached SD card and immediately create annotatable
project assets for every clip on it — before any bytes upload — then bind those
assets to real media objects as uploads complete.

## Principal invariant

This task is correct when three minutes after inserting a card, every clip is
visible as an annotatable project asset on phone, web, and desktop — and notes
written against those assets survive intact when media later arrives, because
`projectAssetId` is allocated at inventory time and never changes.

## Inputs and outputs

Inputs:

- T01-T002 card identity derivation and transfer session model
- T01-T005 server-origin project creation
- T01-T006 mobile app foundation with reliable local operation queue
- S01-T008 sync protocol

Outputs:

- Security-scoped external-volume access and card enumeration on iOS
- Card identity derivation matching T01-T002's server-side definition
- Provisional project asset creation as ordinary project operations
- The media link operation, emittable by both server and desktop
- Server-side link emission on ingest
- Desktop-side link emission for locally ingested media
- Unlinked-asset presentation on phone, web, and desktop
- Re-inventory of a partially uploaded card

## Frozen decisions

1. `projectAssetId` is allocated by the phone and is stable from inventory through linking.
2. Provisional asset creation and linking are ordinary immutable operations.
3. The link operation is idempotent; replaying it changes nothing.
4. Linking an asset already linked to different media is a conflict, not a silent overwrite.
5. Notes, labels, and removal written before linking survive linking without rewrite.
6. Card identity derivation is shared between phone and server so both agree.
7. The card is opened read-only. Nothing is ever written to it.
8. Inventory of a large card is bounded in memory and resumable.
9. Inventory results persist locally and survive volume removal and app restart.
10. Deduped uploads still produce a link operation so the asset gains media.
11. An asset whose upload is abandoned remains a valid annotatable asset, not an error.

## Owned paths

- **Implementer:** `core/rcdvlog-core/` (provisional asset and link operations), `core/rcdvlog-sync/` (operation DTOs), `core/rcdvlog-server/src/transfer.rs` (link emission on ingest), `apps/desktop/src-tauri/` (link emission for local ingest), `apps/mobile/` (external-volume access, enumeration, local persistence)
- **Independent tester:** `apps/mobile/` physical-device card inventory and note-survival tests
- **Integrator:** `apps/desktop/`, `apps/web/` (unlinked-asset presentation on all three surfaces)

## Required interface

- Provisional asset operation type in shared DTOs
- Link operation type (emittable by server and desktop)
- Card identity derivation shared between phone and server
- Unlinked-asset presentation contract for all three UI surfaces

## Acceptance oracle

1. Inventorying a card creates a project asset for every media file on it.
2. Provisional assets appear on phone, web, and desktop before any upload.
3. Notes and labels can be written against a provisional asset immediately.
4. Those notes survive linking, unchanged and unrewritten.
5. `projectAssetId` is identical before and after linking.
6. The server emits a link operation when an upload ingests.
7. A deduped upload still produces a link operation.
8. The desktop can emit a link operation for media it ingested from the card locally.
9. Replaying a link operation is a no-op.
10. Linking to different media than an existing link is a surfaced conflict.
11. Re-inventorying a card adds new footage and preserves existing assets and progress.
12. Card identity computed on the phone matches the server's derivation.
13. Inventorying a card with thousands of files is memory-bounded and completes.
14. Inventory survives app restart and volume removal.
15. Nothing is written, renamed, or deleted on the SD card.
16. Unlinked assets render coherently on all three surfaces with no broken player.
17. An abandoned upload leaves a valid annotatable asset.

## Independent test charter

Try to falsify: "notes survive linking." Use a real SD card with real camera
media. Write notes against provisional assets at inventory time, complete
uploads, and assert byte equality of every note after linking. Test volume
removal mid-enumeration on a physical device. Assert card read-only behavior by
comparing a full checksum manifest before and after a complete cycle. Test a card
with thousands of files for memory bounds.

## Review charter

Risk domain: asset identity stability and note preservation. Questions: (1) Does
`projectAssetId` change between inventory and linking? (2) Are notes rewritten
during linking? (3) Does card identity computed on the phone match the server? (4)
Is anything written to the SD card? (5) Do unlinked assets render broken players
on any surface? Severity: P0 for `projectAssetId` change (orphans notes), SD card
writes, or note loss during linking; P1 for broken unlinked-asset presentation or
card identity mismatch. Approval vocabulary: `APPROVE`,
`APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Upload execution, queue policy, and prioritization (T01-T008)
- Media playback and caching (T01-T008)
- Reading media metadata from the card beyond filesystem attributes
- Writing anything to the SD card

## Stop conditions

Stop and escalate when:

- `projectAssetId` cannot be allocated by the phone and preserved server-side
- The link operation cannot be made emittable by both server and desktop
- Card identity derivation diverges between phone and server
- External-volume access cannot be made security-scoped on iOS

## Commands

```sh
cargo test -p rcdvlog-core
cargo test -p rcdvlog-sync
pnpm --dir apps/mobile typecheck
pnpm --dir apps/desktop build
pnpm --dir apps/web build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

T01-T008 layers the upload queue on top of this inventory. The inventory
persistence schema defined here is what the queue reads, so design it for the
queue's access patterns: filtering by state, sorting by policy, and paging by
card.
