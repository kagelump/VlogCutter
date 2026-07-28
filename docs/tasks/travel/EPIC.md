# T01: Travel Ingest

<!-- bounded-epic-metadata
{
  "schema_version": 1,
  "epic_id": "T01",
  "title": "Travel Ingest",
  "base_branch": "main",
  "task_branch_template": "codex/{task_id_lower}-{slug}",
  "tasks_per_cycle": 1,
  "merge_requires_user_authorization": true
}
-->

## Outcome

Deliver a travel workflow in which a phone acts as SD-card courier and annotation
terminal while the home server ingests, verifies, archives, and processes
originals. The user can travel without a laptop, archive footage from hotels
across several unreliable nights, and annotate clips while their originals are
still uploading.

The phone is a courier and a review terminal. It is never a third archive, and
uploading is never treated as permission to reuse a card.

## Frozen context

- Product contract: `docs/tasks/travel/ARCHITECTURE.md`
- Architecture contract: `docs/tasks/travel/ARCHITECTURE.md`
- Builds on: `docs/tasks/server/EPIC.md` (companion server, complete)

### Architectural constraints

- The global 64 MB HTTP body limit is unchanged; chunk routes carry their own per-route limit.
- The server's loopback-only listener is unchanged; remote access is Tailscale Serve.
- Existing NAS scan roots remain `immutable_original` and read-only.
- The archive root is writable only along the create-new-file path.
- Partial uploads never appear beneath the archive root.
- Staging never lives on the NAS.
- The chunk ledger, not the staging file, is the restart-time source of truth.
- `cardRelativePath` is untrusted input and is containment-checked after canonicalization.
- Provisional assets and link operations are ordinary project operations, not a parallel model.
- `projectAssetId` is allocated by the phone at inventory time and never changes.
- The link operation is emittable by the server *and* the desktop.
- The phone never becomes an authority for original bytes.

### Companion-server foundation (implemented)

| Needed | Task | Status |
| --- | --- | --- |
| Desktop pairing, root mapping, baseline adoption | S01-T010 | complete |
| Web library and project browsing | S01-T014 | complete |
| Web proxy review and annotation | S01-T015 | complete |
| Authentication and secure transport | S01-T009 | complete |

## Queue

| Order | Task | Title | Depends on | Status |
| ---: | --- | --- | --- | --- |
| 10 | [T01-T001](tasks/T01-T001-writable-archive-root-and-commit-path.md) | Writable archive root and atomic commit path | — | ready |
| 20 | [T01-T002](tasks/T01-T002-resumable-transfer-protocol.md) | Resumable transfer sessions, chunking, and verification | T01-T001 | ready |
| 30 | [T01-T003](tasks/T01-T003-archive-ingest-and-dedupe.md) | Archive-to-library ingest and content dedupe | T01-T002 | ready |
| 40 | [T01-T004](tasks/T01-T004-mobile-proxy-recipe.md) | Low-bandwidth mobile proxy recipe | — | ready |
| 50 | [T01-T005](tasks/T01-T005-server-origin-projects-and-desktop-adoption.md) | Server-origin project creation and desktop adoption | — | ready |
| 60 | [T01-T006](tasks/T01-T006-mobile-app-foundation.md) | Mobile app foundation, pairing, sync, and offline notes | T01-T005 | ready |
| 70 | [T01-T007](tasks/T01-T007-card-inventory-and-provisional-assets.md) | Card inventory and provisional assets | T01-T002, T01-T005, T01-T006 | ready |
| 80 | [T01-T008](tasks/T01-T008-transfer-queue-and-phone-review.md) | Transfer queue, priority policies, and phone review | T01-T003, T01-T004, T01-T007 | ready |
| 90 | [T01-T009](tasks/T01-T009-travel-acceptance.md) | End-to-end travel acceptance | T01-T008 | ready |

Cross-epic server prerequisites are documented in each task packet and in the
[companion-server epic](../server/EPIC.md); all server prerequisites are complete.

## Promotion rule

The epic is promoted only when every required implementation, QA, review, and
release-gate packet is complete. One task cycle never promotes unrelated work.

This epic is complete only when all nine task packets satisfy their acceptance
criteria and T01-T009 records evidence from a real multi-night trip simulation
using real camera media over a constrained network. A single small file uploaded
over a fast local link does not constitute completion.

## Cross-cutting verification

```bash
pnpm --dir apps/desktop build
pnpm --dir apps/web build
pnpm --dir apps/mobile typecheck
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm test:integration
```
