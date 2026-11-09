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

- **Movie time** comes only from the shared synchronizer
  (`rcdvlog_core::tv_review_sync`, #223; see [tv-review-sync.md](tv-review-sync.md)).
  `review_sync` assembles its `SyncRequest` from stored session evidence and
  never estimates anything itself:
  - **Capture clocks** come only from `review-capture-calibration.v1` clock
    samples (the recorder's bracketed worklet probes), one per `clockId`.
    Probes of one clock must share a performance epoch, and their origin
    spread is added to the declared tolerance. A clock runs until the next
    clock epoch starts or the recording ends.
  - **Manual cues** are keypresses on the monotonic clock. A cue's capture
    position is its `performanceMs` mapped through its epoch's calibration;
    its declared elapsed movie second becomes raw PTS on the target's exact
    origin and timebase. A cue is a `manualCue` anchor with confidence 0.9 and
    ±0.25 s movie uncertainty (human reaction). Cues without a calibrated
    epoch or outside the clock's range are not used and are listed in
    `sync.diagnostics`.
  - **Capture discontinuities, buffer overflows, device loss and page
    interruptions** are `captureInterrupted` boundaries.
  - The shared default policy applies; in particular there is **no
    single-anchor extrapolation**, so a remark is estimated only between
    consistent nearby anchors.
- Each utterance's complete native `[startFrame, endFrame]` interval is
  queried, split at clock epochs. Its `sync` keeps every returned slice with
  its state (`anchored`, `estimated`, `conflicting`, `unknown`), candidates,
  evidence IDs and diagnostics; `sync.state` is `mixed` when the slices
  differ. `movieTime` is only a locator for the utterance **start**, present
  when the first slice is anchored or estimated with one candidate. Later
  unknown or conflicting parts stay visible and are never smoothed over.
- `movieTimeRanges` projects safe, single-candidate anchored/estimated slices
  into recording and movie seconds on their original clocks. Adjacent spans
  coalesce only when both endpoints meet; unknown/conflicting gaps, seeks and
  clock changes stay separate. A long ASR line may have known timing only in
  its middle: the transcript shows a shot thumbnail from the first known span,
  linked to that video position, with a hover hint that it is the first observed
  playback in the line. Timing diagnostics remain in the workspace data rather
  than visible text below each line. These spans never replace
  the utterance-start locator used for exports or imply word-level timing.
  An explicit human locator correction/clear takes precedence and hides these
  automatic display ranges in the workspace.
- `sync.status` is `pending` (no calibrated clock: nothing is synthesized from
  request start, packet arrival or receive time), `noAnchors`, `mapped` or
  `failed`.
- A **human movie-time correction** is an attributed declaration
  (`source: humanCorrection`), shown alongside, never as measured sync.
- **Candidate cuts** come from the shared `map_to_cuts` over anchored and
  estimated slices only (or the picture at a human-declared movie time).
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
  and run, plus `targetContract`. For `historicalRenderedArtifactReviewV1`
  (#232) the rules add that the movie's current sources, constraints and
  renderability were not established and that the report grants no render,
  edit, activation, upload, source-trim or feedback-apply authority. See
  [the server historical-review policy](tv-review-handoff.md#server-historical-review-policy).
- **Sync status:** the shared synchronizer's status, policy, anchor and
  boundary counts and diagnostics; each quoted utterance carries its `sync`.
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
  - `importId` (`rcdvlog-server/review/{sessionId}/{discussionId}/{itemId}`,
    built from immutable ids only) and `recordOnce`, the exact
    `{ importId, record }` input of `edit-plan feedback record-once`
  - `proposeChange`
  - `humanAccepted`
  - `canonicalNoteId`
  - next-step guidance

**Handoff to the owning catalog** (Mac, CLI):

1. Feed each `recordOnce` to `edit-plan feedback record-once --input`. The
   owning catalog derives the note ID from `importId`, so a retry after a lost
   response returns the original note instead of creating a second one, and a
   changed request under the same `importId` is
   `review_feedback_import_conflict`.
2. POST the returned note ID to
   `…/review-discussion-items/{itemId}/reconciliation`. That receipt is single.
   Repeating the same ID is fine; a different ID is 409
   `review_reconciliation_conflict`. Both steps are therefore safe to retry.
3. Run `propose` and `decide` only for `humanAccepted` items, under the
   existing guards.
4. Stale revisions are handled with the existing `map` there.

The server never writes the project catalog.

## Tests

- **Rust core** (`review_discussions_tests.rs`, on a real #222 session and a
  #224 run with a scripted engine):
  - shared-synchronizer estimates between two calibrated cues, with the
    unknown tail kept as its own slice and no extrapolation after the last
    cue; a cue without calibration leaves everything pending
  - (`review_sync_tests.rs`) a capture interruption between cues is never
    interpolated; each clock epoch is its own capture clock; inconsistent
    calibrations and cues from an uncalibrated epoch are not used
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
- Real conversations, real-device calibration and TV timing accuracy, server-side
  acoustic-marker or passive-soundtrack anchors for stored sessions, and
  bounded raw-audio playback in the
  workspace. The session page offers the full WAV download only.
- Utterance-level grouping quality on real speech.
