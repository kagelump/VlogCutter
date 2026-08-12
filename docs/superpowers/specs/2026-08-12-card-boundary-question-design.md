# Card boundaries: a question, not a warning

Date: 2026-08-12
Follows: `2026-08-12-chronology-visual-desk-design.md`

## Problem

`Catalog::proposed_capture_sequence_splits` reports a filename counter that
restarts mid-sequence. That is what a second SD card looks like. It is equally
what a camera that rolls its own counter over looks like, and swapping several
cards through one camera during a shoot is ordinary practice — so for many real
projects the report is not a fault, it is a question with an ordinary answer.

It was presented as a fault and had one exit:

1. **No way to say no.** Proposals are derived from the facts on every solve and
   have no persisted identity, so "yes, that is just where I changed cards"
   could not be recorded. The only way to stop the warning was to accept a split
   the user might not want.
2. **The answer was two levels from the question.** The worklist row rendered
   `proposal.reason` and, on click, moved the focus. The action lived in the
   inspector's Card tab under "Suggested card boundaries".
3. **It counted as a problem.** Review showed an amber `N chronology problems ·
   Fix` chip and the desk headed the rail "Needs a look", both reporting a fault
   in footage that is usually fine.

## Design

### The question carries its answers

Split proposals leave `actionableProblems` and get their own card at the top of
the rail: the question, the evidence in one sentence, and two buttons.

**Review this boundary on the deck** focuses the first clip after the restart.
The compare deck then shows the last clip of the old run beside it, because the
deck's flanks are solved-order neighbours. A card swap usually looks like a
scene change, so the answer becomes visible rather than deduced. This is the
main reason the question is worth asking on this surface at all.

**Yes — new card** runs the existing exact-split preview and the impact dock
confirms it, like every other chronology write.

**No — one card** records the answer and nothing else. Deliberately no impact
preview: the clips were already in one sequence and stay there, so there is no
placement change to show and a confirm step would be ceremony over nothing.

### Persisting "no"

An operation, not local state: this catalog syncs, and a dismissal living on one
machine would reappear on another. `captureSequenceSplitDeclined` and
`captureSequenceSplitRestored` ride the `captureSequence` aggregate and
materialize into `declined_capture_sequence_splits` (migration 20).

Keyed by `(project_id, capture_sequence_id, at_counter)` and deliberately not by
affected clip identities, unlike a split or a merge. Those need exact identities
because they move clips and a later import can change what a counter predicate
selects. A decline moves nothing; the answer was about the boundary and stays
correct however the clips around it change.

Declines are listed in the Card tab with an **Ask again** control, so a misclick
costs one click.

### Advancing past a settled boundary

Proposal derivation scans for the first restart that has **not** been declined,
rather than the first restart. Filtering after the scan would leave a three-card
trip silent after one answer instead of surfacing its second boundary.

### Framing

`chronologyProblemCount` stops counting proposals. A new
`chronologyQuestionCount` drives a neutral cyan `N card boundaries to confirm ·
Review` chip, shown only when there are no real problems. The desk header reads
"1 card boundary to confirm" instead of claiming the order is trustworthy.
Contradictions keep the amber warning chip — those are genuinely wrong.

## Testing

- `core/rcdvlog-core/tests/chronology_api.rs` — declining stops the proposal;
  restoring brings it back; declining one boundary reveals the next restart;
  declining twice is idempotent.
- `src/test/cuj/chronology.cuj.test.tsx` — both answers present; the deck pairs
  the boundary clips; "no" writes without a preview; the answer is reversible;
  "yes" still previews before writing; neither Review nor the desk calls it a
  problem, while a contradiction still is.
- `src/test/cuj/contract.test.ts` — the Rust `ChronologyOverview` struct and the
  TypeScript interface declare the same fields.

## Two defects found by running against the real project

Neither was reachable from the harness.

**The Tauri overview DTO never carried `declinedSplits`.** `invoke<T>()` is an
unchecked cast, so the compiler saw nothing, and the CUJs passed because the
harness builds its payloads from the TypeScript type and therefore always agrees
with it. At runtime the field was `undefined` and reading `.length` blanked the
app. The contract ratchet now compares the two definitions directly.

**The new table broke the server's online backup test, and the cause was real.**
Adding one table moved the fixture from 127 to 130 pages, past
`BACKUP_PAGE_STEP`. Instrumenting the copy showed it stalling at `remaining=2`
for 4,599 steps: the 10ms pause between steps is long enough for a concurrent
writer to dirty pages again, so a backup cannot finish against a steadily
written catalog larger than one step. The test had been passing on one page of
margin. The copy now escalates to a single full-database step once it stops
lowering the page count, and the fixture is padded past one step so the test
keeps exercising that rather than depending on schema size.
