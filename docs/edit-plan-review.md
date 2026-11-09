# Edit plan: inspection, review and feedback

How a cut is inspected and reviewed: bounded source-window inspection, whole-cut context excerpts, short comparisons, audiovisual review coverage and revision-bound feedback. Nothing here activates a plan or certifies a source range; human decisions are recorded explicitly. Building plans: [edit-plan.md](edit-plan.md). Renders and bundles: [edit-plan-output.md](edit-plan-output.md).

## Bounded source-window inspection

Use the same Rust `Catalog::inspect_source_window` primitive from the CLI,
desktop adapter, and a future bounded reviewer. It is local media work: it
never calls a model, transcribes, chooses a fallback, accepts a trim, marks a
defect, changes rotation or removes footage.

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan inspect --input /absolute/inspection.json
```

```json
{
  "assetId": "source-asset-id",
  "representation": "original",
  "purpose": "proposedBoundary",
  "sourceInSeconds": 1.0,
  "sourceOutSeconds": 2.0,
  "beforeSeconds": 0.25,
  "afterSeconds": 0.25,
  "sampleCount": 8,
  "sourceTimesSeconds": [1.25],
  "maxWidth": 768,
  "includeAudio": true,
  "includeTranscript": false
}
```

The candidate range is inclusive-in/exclusive-out. Handles expand its inspected
context; unavailable handles fail instead of silently shrinking. Limit the
whole window including handles to 60 seconds, total requested frames to 64,
and the maximum image dimension to 160–1280 pixels. JSON input is at most
64 KiB. Images preserve aspect ratio without upscaling. `sampleCount` distributes
requests across distinct decoded frames in the window. Additional
`sourceTimesSeconds` select the nearest available frame inside the window.
Requested times remain in the request; frame evidence reports the actual PTS.

Select `original`, `generatedProxy`, or `cameraProxy` explicitly. Offline or
incompatible selected media fails. Generated proxies must pass the shared
current-v3 analysis-source validation; legacy/unmapped camera sidecars fail.
Generated proxies have seconds correspondence, not a measured original-frame
map. Camera sidecars carry their verified nominal reduction/mapping. Exact
original-frame evidence requires the original. A denser request cannot recover
discarded frames. `availableFrameCount`, measured minimum/maximum available
frame intervals, returned distinct frames, and maximum unsampled gap expose
these limits; no sparse pack certifies intervening frames.

`ioTimeoutSeconds`, `probeTimeoutSeconds`, and `decodeTimeoutSeconds` are
optional request fields (default 180 seconds each; accepted range 1–600). They
bound selected-representation hashing, each ffprobe process, and each ffmpeg
process. A timeout returns a stable inspection-timeout outcome, kills a stuck
media child, and retains an `attempt.json` checkpoint beside any completed
partial artifacts. A checkpoint is progress evidence, never a completed
inspection receipt. The original is never swapped silently. A caller may use
`Catalog::inspect_source_window_with_policy` with one explicit fallback
representation; only an unavailable or timed-out first representation is
eligible, and proxy requests cannot fall back to originals. Exact-PTS and
fingerprint-bound requests cannot fall back because those identities belong to
the selected representation. The policy result lists each attempted
representation and outcome, and successful evidence states
`actualRepresentation` and its mapping precision. Invalid, stale, insufficient,
or cancelled requests do not trigger fallback.

The result contains `inspection` and `reused`. An inspection binds the complete
selected-representation BLAKE3, indexed source identity, mapping/provenance,
effective orientation, request, media-tool identities and recipe. Frame evidence
includes an ID, integer `representationPts`, rational `mapping.timeBaseNumerator`
and `timeBaseDenominator`, source seconds, zero-based excerpt seconds, and an
image path/hash/length. The representation origin is explicit. Original PTS
are measured; source seconds mapped from proxies carry their weaker precision.
Video and container origins must coincide on the representation time base;
nonzero shared origins map back to source zero. A differing or unknown video
origin fails until an explicit compatible timeline mapping is available.
Rotation uses raw decoded frames with autorotation disabled, then applies the
catalog's effective rotation once. Inspection never infers or stores rotation.

To inspect exactly a previously decoded representation frame, use
`exactRepresentationPts: [12345]` and its
`expectedRepresentationFingerprint`, with `sampleCount: 0`. These are actual
PTS ticks, not frame numbers or guessed model decimals. Unknown PTS or frames
outside the window fail. A fingerprint mismatch reports `stale_evidence`.
Do not equate exact proxy PTS with exact source frames. Proposed cut endpoints
still need the source-frame/audio-sample checks and listening appropriate to
their intended use.

Sound has an independent opt-in. `includeAudio: false` never extracts audio or
reads a transcript. When true, the selected representation supplies stereo
48 kHz PCM WAV context and 20 ms waveform bins with peak/RMS and a low-energy
cue below −50 dBFS RMS. Source bounds round inward to complete 48 kHz decoded
samples; each bin reports excerpt timing. Low energy is neither verified
silence nor speech/quality evidence. A missing audio stream is `noAudioStream`;
a known leading stream gap is retained on the source clock and reported as
`completeWithLeadingGap` with `streamSourceInSeconds` and `leadingGapSeconds`;
failed or truncated extraction is an error, never successful silence or a
complete playable excerpt. No other representation supplies missing sound.

`includeTranscript: true` additionally requires audio opt-in. Only current
accepted segment text matching the selected generated proxy's fingerprint and
the supported local transcript recipe is reused. Original/camera inspections
have no implicitly borrowed transcript. Utterances retain their full source
segment bounds, clipped excerpt intersection and `clippedByWindow`. Word
alignment is unavailable and is stated explicitly. No targeted transcription or
alignment runs as a side effect. Segment boundaries and waveform cues cannot
establish complete words; listen before accepting speech trims.

`purpose` records `locator`, `proposedBoundary`, or `inspectedBoundary` intent.
All results say what was inspected and what remains uncertain. They never become
an accepted human constraint or confirmed covered/dark/unusable interval by
being cached. Maps, meaningful dark scenes, hands and close-ups require context
and human judgment. [Confirmed human range decisions](edit-plan.md#confirmed-human-source-ranges) consume this
evidence through strict freshness checks. Bounded provider navigation is a
separate consumer that is not wired here.

Completed receipts live in canonical SQLite `source_inspections`; files live
under the catalog's managed `cache/source-inspections`. Reuse requires identical
identity JSON and verified artifact contents. Range/handles, sampling, sound,
current accepted transcript, representation bytes, mapping, orientation, recipe
or tool changes invalidate reuse. Damaged/missing files rebuild in a unique
attempt directory, leaving earlier evidence intact. A persistent per-evidence
lock and the shared host-heavy lease protect ownership; cancellation is checked
during hashing, media children, waveform work, verification and before durable
publication. Failed/cancelled attempts never acquire a completed receipt. The
current CLI/desktop adapters do not expose a cancellation control; the domain
callback is available to an owning run controller. Local receipts are derived,
node-local evidence and do not add human facts to the sync operation stream.

### Bounded reviewer-requested inspection session

An external host or reviewer can request more local evidence through a
canonical, bounded session. The CLI only runs the typed local tools; it does
not run a model, contact a provider, admit paid work, or write human decisions.
Create requires a current seed inspection and freezes its asset, source and
representation fingerprints, inspected window, mapping, and current human
constraint revision:

```bash
rcdvlog --catalog catalog.sqlite edit-plan review-session create --input session.json
rcdvlog --catalog catalog.sqlite edit-plan review-session step --input step.json
rcdvlog --catalog catalog.sqlite edit-plan review-session status --session SESSION_ID
rcdvlog --catalog catalog.sqlite edit-plan review-session steps --session SESSION_ID --limit 32
rcdvlog --catalog catalog.sqlite edit-plan review-session finish --session SESSION_ID
```

`session.json` contains `seedEvidenceId`, `assetId`, `maxSteps` (1–32),
`maxTotalFrames` (1–512), `maxMediaSeconds` (at most 600, including handles
and failed attempts), `maxWidth` (160–1280), and explicit `allowAudio` and
`allowTranscript` booleans. Each step JSON contains `sessionId`, sequential
`stepIndex`, `caller` (`host` or `reviewer`), and a tagged request, for example
`{"tool":"inspect","input":{"assetId":"...","representation":"original","purpose":"locator","sourceInSeconds":1.0,"sourceOutSeconds":2.0,"sampleCount":4,"maxWidth":640}}`.
Other tools are `suggestTrim` and `observe`; observations require cited
inspection evidence, bounded text, uncertainty, and a source interval. Steps
are atomically admitted and durably charged against session limits before local
work; identical completed steps can be read again, while failed/interrupted
attempts consume their slot. `status` and paged `steps` are read-only. `cancel`
may be issued from another process while a step is running; `finish` rechecks
source and constraint freshness. Stale source or constraints stop the session.

Reviewer observations and trim suggestions remain advisory. No session step can
confirm a source range, classify a defect as fact, or change a plan. Human
decisions continue through the revision-bound range commands above. Sessions
reuse canonical inspection artifacts and SQLite receipts; they are not a
provider-accounting ledger or a general agent runtime.

Generated numbered-frame/tone tests verify mechanics and mappings. They do not
establish natural speech, visual defect classification, editorial timing or
story quality. Record a separate listening/playback review for those judgments.


## Whole-cut context excerpts

Render a short contiguous neighborhood from an immutable edit-plan revision
without creating or activating another revision:

```bash
rcdvlog --catalog catalog.sqlite edit-plan excerpt --input excerpt.json
```

The `PreviewExcerptRequest` input is capped at 64 KiB:

```json
{
  "revisionId": "edit-plan-revision-id",
  "maxHeight": 360,
  "scope": {
    "firstOperationId": "cut-before",
    "lastOperationId": "cut-after"
  }
}
```

The operation IDs select 1–12 contiguous whole cuts in their immutable revision
order. The decoded excerpt must be no longer than 60 seconds. The renderer uses
the existing preview artifact, cancellation, validation, source identity,
constraint, and publication owners. An excerpt is a local review artifact; it
does not edit, select, activate, or otherwise mutate the plan.

Picture contains only the selected neighborhood. Audio is evaluated on the
full revision timeline: every split/J-L or linked-source event that overlaps
the excerpt interval is considered, including an event whose picture cut lies
outside the selected neighborhood. The mixer crops those events at the
excerpt's full-timeline sample boundaries, rebases them to the excerpt, and
preserves their original fade phase. Source in/out handles stay those declared
by each immutable operation; excerpt selection does not extend source media
ranges. Missing source handles or invalid full-plan constraints still fail the
render.

The excerpt starts at the selected cuts' cumulative planned preview-frame
boundary on the full revision clock (30000/1001 fps). The encoded excerpt then
has its own zero-based decoded movie clock. Source locators in result mappings
remain tied to the selected source clocks and should not be treated as exact
source-frame identity; preview resampling and frame holds do not restore source
frames. This is context for review and comparison, not a frame-accurate source
cut or proof of pacing, continuity, complete words, or editorial acceptance.

The artifact identity binds the full immutable revision, excerpt scope,
contributing picture and audio sources, source constraints, tool identities,
and rendering recipe. Later comparisons should use completed excerpts with
matching current identities; stale source or constraint evidence requires a
fresh render. Existing `edit-plan preview-status`, `preview-list`, and
`preview-cancel` commands observe or stop the owned artifact.

### Assemble evidence for a reviewer

After rendering and verifying an excerpt, assemble it with explicit intent and,
optionally, bounded source handles. This produces durable evidence; handles are
separately playable source inspections and never extend or alter the movie.
There is no provider transport in this workflow.

```bash
rcdvlog --catalog catalog.sqlite edit-plan preview-status --artifact <artifact-id> --verify
rcdvlog --catalog catalog.sqlite edit-plan context create --input context.json
rcdvlog --catalog catalog.sqlite edit-plan context get --evidence <evidence-id>
```

The create request is capped at 64 KiB. It identifies a completed, verified
`cutNeighborhood` artifact and supplies an editorial goal instead of asking
the system to infer one. Source handles are optional `SourceInspectionRequest`
objects tied to media that overlaps the rendered picture or sound. Their
combined requested spans and before/after handles must fit the declared
`maxHandleMediaSeconds` cap (at most 120 seconds); there can be at most 24.

```json
{
  "artifactId": "preview-artifact-id",
  "intent": "Keep the complete movement and reaction while preserving the spoken line.",
  "sourceHandles": [],
  "maxHandleMediaSeconds": 0
}
```

The returned context includes the verified movie and cut validation, immutable
input identity, full-timeline origin and clock precision, declared 48 kHz audio
intervals overlapping the excerpt (including J/L events whose picture is
outside it), handle inspection results, and unresolved questions. A handle can
be explicitly unchecked; inspect that state before using it as evidence. Movie
and audio mappings are locators, not word alignment or exact source-frame
identity. `context get` reads historical evidence without certifying that the
media remains current.

Review the movie and handles yourself, then record an observation against one
of the context's cut operation IDs:

```bash
rcdvlog --catalog catalog.sqlite edit-plan context note --evidence <evidence-id> --input note.json
```

The note JSON uses the existing `RecordReviewNoteRequest` fields (`projectId`,
`revisionId`, `operationId`, `author`, `category`, `observation`,
`viewedArtifactId`, `movieTimeSeconds`, and `evidenceRefs`). Use the context's
project/revision/artifact identifiers and a cut operation ID from its
validation. The wrapper binds the note to the viewed artifact and evidence ID
and checks they are still current. A human may then use existing feedback
`propose`, `decide`, `map`, and `apply` commands to recommend a typed change,
accept or decline it, resolve changed targets, and create an inactive draft.
Ordinary revision guards and validation still apply; activation remains a
separate explicit command. These commands use the bounded local inspection
tools and constraints. They do not invoke a cloud agent or upload media.


## Short local edit comparisons

Short comparisons package two to four named variants from one project. Each
variant selects one to twelve contiguous whole cuts from an immutable revision;
each rendered excerpt is limited to 60 seconds. The renderer reuses the normal
preview, source identity, audio, validation, cancellation and artifact owners.
The comparison creates a local HTML player that loads variants sequentially in
one video element and can repeat the sequence. Native video controls keep audio
available. It has no autoplay or external assets.

### Create and inspect

The request is capped at 64 KiB. Variant names must be unique and unambiguous;
every revision must belong to the named project.

```bash
rcdvlog --catalog catalog.sqlite edit-plan compare create --input comparison.json
rcdvlog --catalog catalog.sqlite edit-plan compare status --comparison <comparison-id>
rcdvlog --catalog catalog.sqlite edit-plan compare status --comparison <comparison-id> --verify
rcdvlog --catalog catalog.sqlite edit-plan compare cancel --comparison <comparison-id>
```

```json
{
  "projectId": "project-id",
  "intent": "Compare the opening movement before choosing a cut.",
  "maxHeight": 360,
  "variants": [
    {
      "name": "Current timing",
      "revisionId": "revision-a",
      "scope": {"firstOperationId": "travel-a", "lastOperationId": "arrival-a"}
    },
    {
      "name": "Earlier departure",
      "revisionId": "revision-b",
      "scope": {"firstOperationId": "travel-b", "lastOperationId": "arrival-b"}
    }
  ]
}
```

Creation is sequential and reports JSON progress on stderr, including the
variant name. `cancel` sets a durable cancellation request and asks the active
preview owner to stop. An interrupted or failed comparison is not a playable
complete result; resume by creating a new explicit comparison. The default
status call reads the receipt without hashing media. `--verify` rechecks current
source/output identity for every preview and the standalone player bytes. A
false or missing `currentForRequest` requires a fresh comparison before a
preference can be recorded.

The player path is in the comparison receipt. Open it locally in a browser,
choose a named variant, or use “Play sequence (all variants)” to play A, B, and
any remaining variants in request order. “Repeat sequence” repeats them from
the first variant; “Stop” halts playback. The player records no listening
attestation. Compare actual endpoints, movement, reaction, sound and pacing
yourself.

### Record a human preference

Preference is a separate, explicit human command. It requires a complete,
currently verified comparison, names the preferred variant and its expected
revision, and guards the project's active revision. Use the literal `none` when
there is no active revision.

```bash
rcdvlog --catalog catalog.sqlite edit-plan compare prefer --input preference.json
rcdvlog --catalog catalog.sqlite edit-plan compare choice --choice <choice-id>
rcdvlog --catalog catalog.sqlite edit-plan compare apply --choice <choice-id> --expected-active none
```

```json
{
  "comparisonId": "comparison-id",
  "preferredVariant": "Earlier departure",
  "expectedPreferredRevisionId": "revision-b",
  "expectedActiveRevisionId": "none",
  "decisionOwner": "human",
  "rationale": "I listened to the variants and prefer the earlier complete movement.",
  "retainOperationIds": [],
  "createDraft": false
}
```

The preference writes an ordinary revision-bound feedback note against a cut
visible in the preferred variant. `retainOperationIds` optionally adds explicit
accepted picture-and-audio retention choices, limited to cuts actually shown in
that preferred excerpt. An empty list adds no retention locks. `createDraft`
may ask the same command to create a normal inactive child draft; otherwise use
`apply` after reviewing the durable choice. Draft creation uses ordinary
revision and human-choice guards and never activates the result.

The excerpt recipe preserves the authored final source-sound tail when the
last picture boundary rounds slightly earlier on the preview frame grid. The
movie's measured duration can therefore include a short sound-only tail; cut
picture mappings continue to end at their decoded frame boundaries.

Accepting retention changes the human constraint identity, so earlier previews
may cease to be current. Applying that recorded choice rechecks the exact viewed
winner's bytes, media and intent and validates it against current human rules;
it does not relabel the earlier render as current under its former constraints.

`choice` reads the durable preference and any linked draft receipt. `apply` is
idempotent: if the receipt already names a draft, it returns that draft rather
than creating another. If the active revision changed, the stale compare-and-set
guard refuses to overwrite it; a preference note remains recorded, and the
choice receipt reports any draft failure. Variant bytes and source identities
must still verify before preference or application. These records do not claim
that the selected version has natural pacing or completed human review beyond
the explicit rationale provided.


## Audiovisual review coverage

Schema 64 stores review coverage independently from edit-plan feedback. A
coverage record identifies the project, immutable revision, stable cut ID,
verified preview artifact, actor, method, record time, evidence references,
uncertainty, and one or more intervals on the artifact's movie clock. Every
interval names a modality (`picture` or `listening`) and whether it is
`continuous` or `sampled`. The ledger is append-only evidence; changing a plan
or reviewing a different artifact creates additional records.

An explicit playback caller may attest to human review only with
`actor: human`, `method: humanPlayback`, and
`explicitHumanPlayback: true`. The catalog requires a completed preview whose
output, inputs, constraints, and renderability pass fresh verification before
it accepts that attestation. `agentReview`, `modelReview`, `sparseSampling`,
`transcriptInspection`, `decodeCheck`, and `energyCheck` can be recorded as
other evidence, but cannot count as human picture or listening coverage. The
explicit flag is an assertion by the caller; the catalog cannot observe a
person watching or listening.

Intervals are bounded by the verified movie duration and must overlap the
artifact mapping for the named cut. An excerpt-local movie locator is rebased
onto its full revision clock origin before it enters the report or next-review
queue. Without a full preview mapping, times use the planned 30000/1001 frame
clock and the report labels that as an estimate. Reports compute per-cut picture and
listening coverage independently. Only continuous human playback intervals
close a gap. Sampled intervals are unioned and shown as sampled seconds, while
the remaining gaps become a bounded next-review queue (at most 100 items).
Repeated or overlapping intervals do not inflate covered duration.

Coverage remains attached to its original revision and artifact. Reports may
carry an interval to a later revision only when the stable cut and its previous
and next cut snapshots, source ranges, source reuse, contextual audio digest,
source fingerprint, orientation, and selected representation fingerprint all
match. The interval is stored relative to its mapped cut boundary, so a prefix
addition can shift its movie location without changing the reviewed cut.
Changed dependencies leave the old record historical and open a new gap. The
`verify` report option rechecks referenced old and current artifacts and
excludes evidence whose artifact is no longer current; without it the report is
a historical ledger view and does not certify current media bytes. A report
examines at most 1,000 recent historical records and marks truncation.

The report is not a claim about comprehension, complete dialogue, source-frame
identity, editorial acceptance, pacing quality, or meaningful listening. The
movie intervals locate a human playback assertion in a rendered preview, whose
source-frame sampling may include resampling and holds. Feedback notes,
proposals, decisions, source inspections, and plan activation keep their
existing separate owners.

CLI adapters should expose bounded JSON requests and paginated history, using
the core methods `Catalog::record_review_coverage`,
`Catalog::review_coverage_report`, and `Catalog::review_coverage_records`.


## Revision-bound edit-plan feedback

Review feedback is a bounded, portable operation stream attached to immutable
edit-plan revisions. It stores editorial observations and explicit responses
to those observations; it is not a general story-beat or script store. The
fact kinds remain separate: an immutable **note** records an observation,
a **proposal** suggests a typed edit, and a **human decision** accepts or declines
that proposal (or retains an existing cut). A decision in the `applied` state
records a host receipt of a prior human acceptance. A target **mapping** is an
explicit human fact; it does not rewrite the note or proposal.

All feedback CLI commands require an existing explicit catalog. `record`, `record-once`,
`propose`, `decide`, `map`, and `apply` are writes; `get` and `report` open the
catalog read-only and do not migrate it. JSON inputs are bounded to 64 KiB and
reject unknown fields.

For an external review item, use `edit-plan feedback record-once --input
/absolute/import.json` with `{ "importId": "issuer/session/discussion/item",
"record": <RecordReviewNoteRequest> }`. Retain the same namespaced stable item
identity through retries. The canonical note ID is derived from that identity
and project ID; the normal portable note fact is committed atomically, without
a second receipt store or schema migration. The result contains `importId`,
`noteId`, `duplicate` and the current `feedback` view. After a lost response,
retrying returns the original note, including its original viewed-movie evidence,
even while that media is offline. Concurrent retries create one note fact.

Reusing an import identity with any changed request content returns
`review_feedback_import_conflict`. Do not key imports by mutable text or list
position. An explicitly revised external item needs its own immutable identity
and reconciliation decision; no retry overwrites a canonical note, proposal or
human decision. A first import keeps all existing target/revision/receipt guards.
The existing `record` command continues to create a new note each time.
Neither command proposes, accepts, applies or activates an edit. Return the
committed `noteId` to the external item's reconciliation endpoint only after
the CLI succeeds; a lost reconciliation response can safely retry both steps.

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan feedback record --input /absolute/note.json
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan feedback propose --input /absolute/proposal.json
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan feedback decide --input /absolute/decision.json
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan feedback map --input /absolute/mapping.json
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan feedback get --note <note-id> --revision <revision-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan feedback report --project <project-id> --revision <revision-id> --offset 0 --limit 20
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan feedback apply --project <project-id> --expected-base <revision-id> --expected-active <revision-id-or-none> --input /absolute/accepted.json
```

Each mutation request is the core request object itself. For example:

```json
{"projectId":"project_1","revisionId":"revision_a","operationId":"cut_opening","author":"human","category":"endpoint","observation":"The cut starts after the first step.","viewedArtifactId":null,"movieTimeSeconds":null,"evidenceRefs":[]}
```

```json
{"noteId":"reviewnote_1","expectedRevision":1,"author":"human","change":{"kind":"trim","sourceInSeconds":1.2,"sourceOutSeconds":4.8},"rationale":"Keep the first complete movement."}
```

```json
{"noteId":"reviewnote_1","expectedRevision":2,"proposalId":"reviewprop_1","decisionOwner":"human","action":"accepted","rationale":"I reviewed and accept this trim.","hardChoice":null}
```

```json
{"noteId":"reviewnote_1","expectedRevision":3,"proposalId":"reviewprop_1","revisionId":"revision_b","operationId":"cut_opening","decisionOwner":"human","rationale":"This stable cut ID is the reviewed target in the new revision."}
```

The note captures an immutable target snapshot from its original revision:
stable operation ID, position, source asset and source in/out bounds, explicit
audio, neighboring cuts, and count of occurrences of the source asset. A bounded
sound-context digest covers declared 48 kHz sound events overlapping that neighbor
window, with 100 ms boundary handles, including sound owned by distant cuts.
`contextAudioPrecision` distinguishes declared timeline identity from unavailable
invalid-draft timing; an unavailable count does not establish silence. Later
views compare this target with the requested revision and report source,
range, audio, order, neighbor, reuse, or missing/split changes. Overlapping
cuts may be returned as bounded candidate operation IDs when a target is
missing; candidates are locators only and are never selected automatically.
If a split, replacement, reuse, or reordered sequence changes what the note
means, explicitly map the note or proposal to a target in a named revision.
Mapping preserves the original note/proposal target and history.

`get` reports `currentRevisionId` for the revision being inspected. Its
`effectiveTarget.revisionId` is the revision where the original note target (or
the explicit mapping) was captured. These can differ: the former scopes current
status flags and overlap candidates; it does not move the historical target.
An accepted decision whose current target changed reports `resolution: "unresolved"`;
the persisted `latestDecision.state` and its history retain the acceptance. Explicit
mapping can resolve the target without rewriting that history. Addressed, deferred
and superseded decisions remain visible alongside their separate target flags.
Feedback history is returned newest-first, at most 20 facts, alongside
`historyTotal` and `historyTruncated`. `report` is paginated (default 20,
maximum 100) and includes `total` and `nextOffset`. On an older catalog without
schema 55, a read-only report returns `schemaReady: false` and no items.

### Preview evidence and playback locators

When recording a note from a preview, set `viewedArtifactId` to the completed
artifact. The recorded evidence identifies its output fingerprint (the movie
hash), time base, and QA cut mapping including exact movie PTS boundaries,
frame count, and the reviewed source cut mapping. This is a receipt of what the
catalog recorded for that artifact. Recording it does not re-hash current
sources or output bytes; use preview status with explicit `--verify` when a
current byte check is needed.

`movieTimeSeconds` is optional and requires `viewedArtifactId`. It is stored as
a **declared playback locator** within the movie duration. It is not verified
against actual playback, an output frame, a source timestamp, or a source
boundary. It does not assert that anyone listened to the audio. Do not derive
source trims from this value; use source inspection and explicit source bounds.

### Proposal, decision and draft apply

Supported typed changes are:

- `trim`: change the reviewed cut's source in/out bounds.
- `replace`: change source asset and bounds, with an explicit audio value.
- `reorder`: move before a stable `beforeOperationId`; `null` appends.
- `omit`: remove the reviewed cut from the draft.
- `audio`: set explicit audio or use `null` to restore linked-source audio.

For example, `{"kind":"reorder","beforeOperationId":null}` appends the cut.
Proposals check typed input and project ownership. Draft application returns
ordinary source-range, operation, and edit-plan validation; a draft with blocking
findings remains inspectable and cannot be activated or rendered. A proposal changes no plan and is not an
acceptance. To apply one or more accepted proposals, the `apply` input is an
object containing **only** `accepted`, an array of
`{"noteId":"...","expectedRevision":3,"proposalId":"..."}` references.
The command's `--project`, `--expected-base`, and `--expected-active` flags
complete the core request. Base must identify an existing revision in that
project; active must be its expected active revision ID or the literal `none`.
Core checks both guards, each note revision and human acceptance, and that the
target still matches the selected base. Stale targets require explicit mapping.

Apply creates a new inactive draft from the explicit base, returns its normal
diff and blocking/advisory validation, and records host applied receipts for
the accepted decisions. It never auto-activates the draft or changes
chronology. Activate it later through the ordinary revision-guarded
`edit-plan activate` command if desired.

### Retaining an existing cut

To retain a cut without accepting a proposal, record an explicit accepted
decision with `proposalId: null` and `hardChoice` set to `keepPicture` or
`keepPictureAndAudio`. This is a hard choice tied to the existing reviewed
operation and source bounds. It is not a proposed edit, and does not create an
allow/exclude source range. The canonical confirmed source-range subsystem
remains the sole owner of allow/exclude constraints; retained choices are an
additional operation-identity policy checked by plan validation.

`keepPictureAndAudio` retains that operation's declared audio fields; sound
owned by other cuts remains tracked context. Before withdrawing a retained choice,
mapping it, deferring it, or replacing it with a soft decision, first record an explicit `superseded` human decision. Hard choices
stay effective until superseded. Accepted proposals are not automatically
permanent locks, and unaccepted proposals are never locks. A normal accepted
proposal can be followed by another human decision without a hard-choice
supersession step.

Each note has at most 256 facts, with capacity reserved for an acceptance,
application receipt and explicit withdrawal so bounded history cannot strand a
retained choice. The service caps review notes at 20,000 per project and
retained choices at 1,024. Old feedback facts remain immutable history; create
a new note when the observation itself changes. Schema-55 operations are
portable typed project operations and older builds keep unsupported feedback
operations quarantined until a build that understands them can redispatch
them.

