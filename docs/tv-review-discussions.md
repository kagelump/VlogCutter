# TV voice review: editorial discussions (#225)

The editorial review workspace turns a [review transcript](tv-review-transcription.md)
into grouped editorial discussions with explicit human decisions. It then
exports exactly what the owning catalog's existing `edit-plan feedback`
commands accept. It is **not** a second feedback owner. Canonical notes,
proposals, decisions, mappings and `apply` all stay in the project catalog
([revision-bound feedback](edit-plan-feedback.md)).

| | |
| --- | --- |
| Owner | [`review_discussions.rs`](../core/rcdvlog-server/src/review_discussions.rs) (control migration 16) |
| HTTP adapter | [`review_discussion_http.rs`](../apps/server/src/review_discussion_http.rs) |
| Web | *TV review → target → session → Transcript → Open editorial review* |

## Clocks and cut candidates

Every utterance keeps its **recording clock** time.

- **Movie time** comes from the `SessionClockMapping` seam. Today the only
  implementation is the interim `ManualCueMapping`: the reviewer's last
  "first frame" cue, assuming the TV played without pauses. Its uncertainty is
  ±0.75 s plus 0.05 s per minute from the cue. The workspace reports
  `sync.status: provisional`. **Codex's shared #223 synchronizer replaces this
  adapter.** It must not be extended into a second sync engine.
- **No cue and no sync:** `unanchored`. No movie time and no candidates are
  shown.
- **Candidate cuts** come from the shared Rust
  `ReviewTargetHandoff::picture_candidates` over the uncertainty interval.
  They are locators only. When the text appears to refer to the previous or
  next shot ("previous shot", 上一個, 下一個 and similar), the neighbouring
  cut is added as a candidate and the utterance shows a `referenceHint`.
  Nothing is chosen automatically.
- **Explicit cut choice:** a human sets the discussion's target cut (any cut of
  the immutable target revision) or marks it **unanchored**, meaning a general
  remark. A remark without a confirmed cut is never exported as cut feedback.

## Corrections

The machine utterance is never modified. Corrections are append-only rows,
each with its actor, time and an optional reason. The correctable fields are:

- `text`
- `speaker`: `A`, `B`, `unknown`, `overlap`, `tv` or `background`
- `language`
- `movieTime`: a human-set value has zero stated uncertainty and source
  `humanCorrection`
- `excluded`: for example, film dialogue or background speech

The effective view applies the latest correction for each field and shows the
machine text next to any corrected text. Speaker attribution can only be
asserted from a browser session, because device credentials may be agents.

## Discussions and items

**Grouping.** Discussions start as provisional `heuristic` groups (a silence
of 6 s or more breaks a group; excluded utterances are skipped). People can
also create or regroup them, which marks them `human`. Each discussion
collects its speakers, languages and the union of its candidates.

**Item kinds.** Items are append-only:

| Kind | Meaning |
| --- | --- |
| `observation` | What was noticed |
| `candidateEdit` | A suggested change, optionally typed with the core `ReviewChange` (`trim`, `replace`, `reorder`, `omit`, `audio`) and validated against the core type |
| `speakerClaim` | What a speaker asserted |
| `retraction` | Withdraws an observation, claim or candidate edit. The withdrawn item stays visible and is marked `retracted` |
| `note` | Free note |
| `decision` | `accepted`, `declined` or `deferred` on an observation or candidate edit. **Human only**, also enforced by a schema `CHECK` |

**Rules.**

- **Agent items are provisional.** An agent (device credential) may add
  observations, candidate edits, claims, retractions and notes. These are
  always `provisional` and can never be decisions or human-authored. The
  browser session is the only actor that may assert human authorship.
- **Retracted items cannot be accepted** (409 `review_item_retracted`).
- **Accepting a candidate edit needs a confirmed cut** (409
  `review_target_unconfirmed`).
- **One accepted edit at a time.** At most one candidate edit per discussion
  may be accepted (409 `review_conflicting_acceptance`). Decline it, or retract
  and re-propose, so accepted edits never contradict each other.

## Report and reconciliation

`GET …/review-transcriptions/{runId}/report` returns a bounded, deterministic
`review-report.v1`. It contains:

- **Identity:** project, revision, viewed artifact, target digest, session
  and run.
- **Sync status:** whether movie time is provisional or unanchored.
- **Coverage statement:** `audiovisualReviewAttested: false`. A recording is
  not a continuous-review attestation (#204).
- **Discussions:** quotes with raw and display text, the machine text, the
  correction flag and both clocks, followed by every item.
- **`unresolved`:** discussions without a confirmed cut.
- **`feedback`:** one entry for each non-retracted observation or candidate
  edit in a discussion with a confirmed cut. Each entry contains:
  - `record`: a value of the **core `RecordReviewNoteRequest` type**. Its
    `author` is `human` only for human items; agent items use `reviewer`.
    It carries `viewedArtifactId`, `movieTimeSeconds` (a declared locator),
    and at most 16 evidence refs naming the session, run, discussion, item
    and utterances.
  - `proposeChange`
  - `humanAccepted`
  - `canonicalNoteId`
  - next-step guidance

**Handoff to the owning catalog** (Mac, CLI):

1. Feed each `record` to `edit-plan feedback record --input`.
2. POST the returned note ID to
   `…/review-discussion-items/{itemId}/reconciliation`. That receipt is single.
   Repeating the same ID is fine; a different ID is 409
   `review_reconciliation_conflict`. This prevents duplicate notes after a lost
   response, because `record` itself has no idempotency key.
3. Run `propose` and `decide` only for `humanAccepted` items, under the
   existing guards.
4. Stale revisions are handled with the existing `map` there.

The server never writes the project catalog.

## Tests

- **Rust core** (`review_discussions_tests.rs`, on a real #222 session and a
  #224 run with a scripted engine):
  - the manual-cue estimate and its candidates; past-the-movie and no-cue
    cases are unanchored
  - append-only corrections, a "previous shot" neighbour candidate, and human-only speaker attribution
  - the **two-reviewer retraction flow**: "keep it longer" → "no, keep the
    duration, change the transition". The retraction is preserved,
    retracted and inconsistent acceptances are refused, and a target is
    required. The exported `record` deserializes into the core
    `RecordReviewNoteRequest`.
  - agents cannot decide or claim to be human
  - single reconciliation receipts
- **HTTP adapter:** device credentials can never assert human authorship.
- **Vitest:** clocks, candidates and provisional agent items; a speaker
  correction call; a refused acceptance explained by its code.
- **Playwright** on a real disposable server, with a seeded scripted
  transcript in English and Simplified Mandarin:
  - Traditional display plus raw text
  - speaker correction and grouping
  - the retraction flow and the refused acceptances
  - picking a target and a single accepted edit
  - the agent report JSON, with one human-accepted `editop_split_a` note and
    no export of the retracted item

## Not verified

- A round trip through a real owning catalog's `edit-plan feedback record`.
  It needs a completed preview artifact, which the cloud environment cannot
  render against private media. Only type compatibility is tested.
- Real conversations, real sync (#223), and bounded raw-audio playback in the
  workspace. The session page offers the full WAV download only.
- Utterance-level grouping quality on real speech.
