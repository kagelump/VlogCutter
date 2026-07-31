# D01-T007 cycle report

## Identity

- Epic: `docs/tasks/desktop/EPIC.md`
- Task: `docs/tasks/desktop/tasks/D01-T007-chronology-domain-model.md`
- Base: `main` at `340a64c83e70d70bd98db73c0823f5dd179c500e`
- Task branch: `codex/d01-t007-chronology-domain-model`
- Contract SHA: `dcd2a5ffbe6009fe930c2a82e482edfa252ebfcc` (first repair; further contract-editor commits below)
- Implementation base SHA: `dcd2a5ffbe6009fe930c2a82e482edfa252ebfcc`
- Implementation SHA: `41de19b9277d01853f2b5e9cbf88fcbe5e74e856`
- Reviewed/tested/gated subject SHA: `7aad382007b4dd53a34a7140c69230cdc1fb02aa`
- Evidence commit SHA: null (bootstrap; backfilled on main after merge)

## Contract validation

Admission passed (`validate_task.py`). Three contract repairs and three contract-editor commits preceded or followed implementation:

1. `dcd2a5f` — `architecture_decisions_frozen` false→true (clerical; the packet is a foundation packet whose four decisions are its own authority — same pattern as D01-T003's repair).
2. `711a9a0` — contract editor publishes `CHRONOLOGY_MODEL.md` (the packet's frozen-contract deliverable).
3. `f29b733` — complete path ownership for actual deliverables (`.gitignore`→implementer, `AGENTS.md`→integrator).
4. `e4e686f` — reviewer P2s: documented §9.1 timezone exclusion decision; carved out the legacy `CLOCK_EVENT_CREATED`→`recalculate_clock` projection write with a D01-T008 replacement note.
5. `d341c29` — froze the re-dispatch seam semantics in `CHRONOLOGY_MODEL.md` (scope, filters, ordering, no-conflict-row disposition) and added it to the "What D01-T008 through D01-T017 may not change" list.
6. `252538b` — closed the final reviewer P2 follow-up: the base-less candidate rule states "no materialized revision of 1 or higher (a baseline row at revision 0 does not disqualify)".
7. `7aad382` — planning reconciliation (packet + epic row → complete) and narrow repair dropping `AGENTS.md` from implementer ownership (schema-version fact is the integrator's deliverable; the implementer never wrote it; the validator rejects the resulting cross-role overlap).

All four decisions were implemented as the packet specified; no product decision was changed by repair.

## Roles

| Role | Agent | Model | Verdict / deliverable |
| --- | --- | --- | --- |
| Contract editor | coordinator (repairs 1–3) + contract editor agent (`711a9a0`) + coordinator as contract editor (4–7) | sonnet / coordinator | contract published, repaired, frozen |
| Implementer | one sonnet agent (all four rounds) | sonnet | DONE each round; commits `bd5163e`, `41de19b`, `d3e1f68`, `183c45b`, `d584044` |
| Independent tester | one sonnet agent (re-dispatched once after a stream-watchdog stall; the first dispatch left no files) | sonnet | PASS on 13 own catalogs; commit `73c1175` |
| Reviewer | one sonnet agent (three rounds) | sonnet | REJECT (2 P1) → REJECT (1 new P1, F3) → APPROVE_WITH_FOLLOW_UP |
| Integrator | integrator agent (`a9eeb3d`: SCHEMA.md rows 7–9, schema version 9, AGENTS.md version fact) + coordinator | sonnet / coordinator | done |
| Coordinator (this report) | — | — | dispatch, path enforcement, gates, evidence |

Transparency note (per cycle-skill §13): three contract-editor steps (4–7) were performed by the coordinator directly rather than a dispatched contract-editor agent — the timezone-exclusion decision, the two wording repairs, the seam-semantics freeze, and the planning reconciliation. Each is a docs-only change to contract-owned paths, was reviewer-prescribed (4, 6) or validator-required (7), and none touched production code.

## Changed paths by role

- **Implementer** (enforced per round, all VALID): `core/rcdvlog-core/src/chronology/mod.rs`, `core/rcdvlog-core/src/chronology/model.rs`, `core/rcdvlog-core/src/migrations.rs`, `core/rcdvlog-core/src/operations.rs`, `core/rcdvlog-core/src/lib.rs`, `core/rcdvlog-core/tests/chronology_migration.rs`, `core/rcdvlog-core/tests/fixtures/chronology/**`.
- **Independent tester** (enforced, VALID): `core/rcdvlog-core/tests/chronology_migration_independent.rs`, `core/rcdvlog-core/tests/fixtures/chronology-independent/**` (16 paths: test file, README manifest, 13 catalogs, media file).
- **Contract editor**: packet file, `docs/tasks/desktop/CHRONOLOGY_MODEL.md`.
- **Integrator**: `docs/tasks/server/SCHEMA.md`, `AGENTS.md`, `docs/tasks/desktop/EPIC.md`, `docs/task-cycles/D01-T007/**`.
- **Final gate** (whole range `340a64c..7aad382`, `--role final --include-untracked`): VALID, 35 paths.

## Independent testing

- Verdict: `PASS`
- Novel findings: none — the principal invariant survived falsification on all 13 independently built schema-8 catalogs (4–500 assets), including every charter-mandated hard case: overlapping counter ranges, dangling `start_filename`, NULL `captured_at_raw`, identical raw+filename across folders, non-ASCII digit-run filenames, huge counter, no-digit-run filenames, and the 500-asset three-camera catalog. Pre-order captured via the production `Catalog::list_assets` query; post-order identical everywhere; first differing index: none. Idempotence clean; quarantine (3 known + unknown future op: known applied, unknown retained+reported, peer cursor advanced, no revision row) verified; revision guard (stale base twice → 2 conflict rows, never silent overwrite) verified; `clock_events` retained byte-identical with legacy replay converging; media file hash unchanged; schema-10 refusal intact; loader deterministic across 32 calls.
- Commands and evidence: `cargo test -p rcdvlog-core --test chronology_migration_independent` 22 passed/1 ignored; re-run on the canonical branch and on every later candidate SHA — always green and untouched by all fix rounds.

## Review

- Verdict: `APPROVE_WITH_FOLLOW_UP` (round 3; the follow-up was closed by commit `252538b`, so `open_findings` records 0/0/0)
- Open P0/P1: `0/0`
- Findings across three rounds:

| Round | Verdict | Findings |
| --- | --- | --- |
| 1 (73c1175) | REJECT | P1 F1: quarantine "applies after upgrade" promised by contract and oracle 9 but unimplemented (no re-dispatch seam; cursor advance + dedupe made it unrecoverable). P1 F2: `captureSequenceMerged` self-merge cascade-destroyed assignments/epochs; `captureSequenceSplit` id collision could misassign assets. P2 ×3 (timezone home; projection wording; cosmetic `created_at`). |
| 2 (e4e686f) | REJECT | F1 and F2 closed (verified). New P1 F3: the seam's per-aggregate candidate filter was too coarse AND its guard-failing candidates wrote phantom `stale_base_revision` rows that deterministically fail `import_project_baseline`'s conflict-count verification — restore broken for any project with a twice-edited chronology fact. P2: `operation_id` ordering not log order. |
| 3 (d341c29) | APPROVE_WITH_FOLLOW_UP | F3 closed: seam pre-checks candidates with a no-record guard variant; no path in the seam can write `operation_conflicts` rows; normal apply path byte-for-byte unchanged; baseline-restore regression test genuine (red on fix parent); HLC ordering in place; contract freezes the seam. P2 follow-up: base-less candidate bullet understated the revision-0-baseline exemption — closed in `252538b`. |

- Smallest acceptable remedies were followed: re-dispatch seam implemented (round 1); distinct-sequence/destination-exists bails (round 1); per-operation `last_operation_id` filter + baseline-eligibility (round 2); no-record guard pre-check + HLC ordering + baseline-restore test (round 3).

## Final gate

On the reconciled subject SHA `7aad382`:

- `check_changed_paths.py --role final --include-untracked` over `340a64c..7aad382`: VALID, 35 paths.
- `cargo test -p rcdvlog-core`: 26+22 passed, 0 failed (lib, migration, independent suites).
- `cargo test --workspace`: 25 suites ok, 0 failures.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo fmt --all -- --check`: clean.
- `grep -n "HashMap\|HashSet" core/rcdvlog-core/src/chronology/*.rs`: 0 matches.
- Sensitive-content / media: no source media path is written by any changed code; the independent tester hashed a fixture media file before/after a full migration + operation cycle — unchanged.

All commands recorded with exit codes in `cycle.json`.

## Planning reconciliation

`reconcile_planning_state.py` VALID (3/3 checks); `epic_queue.py --validate` VALID after the ownership-overlap repair (see Contract validation, item 7). Packet status → `complete`; EPIC.md row 70 → `complete`; `CHRONOLOGY_MODEL.md` "## Frozen interface" + `ChronologyFacts` present; `docs/tasks/server/SCHEMA.md` documents `capture_sequences`.

## Merge

- Authorization: granted by the user for D01-T004 through D01-T007 (local merge only; no push without asking separately).
- Status: pending — subject SHA is the reconciled tip `7aad382`; merge `--no-ff` to `main` and post-merge evidence-SHA backfill (`evidence_commit_sha` + `merge.merge_sha`) follow the evidence commit.

## Workflow observations

- Scope pressure: high but contained. The review loop found two real P1 defects the implementer's and tester's suites could not see (F1's upgrade leg was invisible because every party built catalogs with known types; F3's baseline interaction only shows in `import_project_baseline`, which no prior test exercised with a superseded op). All three fix rounds stayed inside implementer-owned paths; no new dependency, migration, or operation type was added — the packet's stop conditions held.
- Packet sufficiency: strong. Oracle 9 (second-build leg) is what made F1 defensible, and Decision 4's "applies after upgrade" wording made the remedy unambiguous. The one under-specified area was the re-dispatch mechanism itself, which the review loop had to pin down across two rounds (per-aggregate vs per-operation filter; conflict-row disposition) before freezing it in the contract.
- Role isolation: held. The independent tester's catalogs were built from scratch via direct inserts; the reviewer never saw implementer reasoning; the implementer never saw the tester's file. Coordinator performed three contract-editor steps directly (documented above).
- Improvements before next cycle: (1) D01-T005 and D01-T006 merged without committing cycle evidence (`docs/task-cycles/D01-T00{5,6}/` do not exist in `main` at `340a64c` — a process gap from earlier cycles; verify_cycle.py evidence was produced but never landed in the tree). (2) The reviewer's per-aggregate remedy for F1 itself introduced the clip-exception gap and the F3 conflict-row hazard — when a reviewer prescribes a fix, the fix loop should treat the prescription as provisional design, not a spec. (3) A `--role final` validation after ANY contract repair would have caught the `AGENTS.md` ownership overlap at repair time instead of at reconciliation.

## Next task

`D01-T008` (Deterministic chronology solver) — identified only; not started. It is `draft`; per EPIC.md's promotion protocol it is promoted by the contract editor after T007 publishes `CHRONOLOGY_MODEL.md`, which is now committed and frozen. The frozen contract explicitly reserves T008's own solver output interface (`OrderKey`/`ChronologySolution`), so promotion does not require further design.
