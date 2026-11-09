# Edit plan: selection, chronology and source ranges

How a rough-cut edit plan is built: chapter-wide sequence selection, chronology-safe placement, and the human source-range decisions the plan consumes. Plans are immutable revisions; drafts are inactive until explicitly activated. Review and feedback are in [edit-plan-review.md](edit-plan-review.md); previews and delivery in [edit-plan-output.md](edit-plan-output.md). The CLI surface is in [agent-cli.md](agent-cli.md).

## Chapter-wide sequence selection

Schema 56 attaches optional versioned human intent and selection evidence to an
immutable edit-plan revision. The existing plan remains the only timeline.
Selection reads local candidate moments; it never calls a provider or transcribes
media. Editorial omission leaves capture chronology and labeling visibility alone.

### Supported commands

Use an explicit catalog and project discovered with `project list`, `project
status`, `clips list`, and `moments get` (see [the CLI reference](agent-cli.md)).

```sh
rcdvlog --catalog /absolute/catalog.sqlite3 edit-plan selection preview \
  --project PROJECT --input /absolute/selection-input.json \
  --expected-base none --expected-active none
rcdvlog --catalog /absolute/catalog.sqlite3 edit-plan selection create \
  --project PROJECT --input /absolute/selection-input.json \
  --expected-base REVISION --expected-active none
rcdvlog --catalog /absolute/catalog.sqlite3 edit-plan selection get \
  --project PROJECT --revision REVISION
```

`preview` and `get` are read-only, including on older catalogs. `create` records
an **inactive** draft. Explicit scope flags are required even when their value is
`none`. Ordinary diff, validation, preview and guarded activation still apply.
Conflicting drafts remain inspectable; blocking findings prevent rendering or
activation. Source/evidence/policy changes during creation reject the snapshot.
The JSON input is bounded to 8 MiB, 64 sequences, 4,096 source entries and 10,000
candidate moments. Optional greedy comparisons stop at 250,000 evaluations,
report the limit, and defer remaining candidates rather than calling them weak.

### Intent and evidence

`SequenceSelectionInput` has version `1`, `decisionOwner: "human"`, a `goal`,
ordered `sequences`, optional `include`/`exclude`, role `counts`, candidate
`relationships`, and runtime budgets. Set `minimumScore`, `repetitionPenalty`, and
`coverageBonus` explicitly; scores rank proposals rather than certify story value.
Each sequence has its own goal, pacing, source list and optional role/activity
claims, heroes, retained-feedback references and place context.

A source is either `kind: "current"`, with `assetId`,
`expectedMomentIdentity` and optional `candidateIds`, or `kind: "declared"`, with
`assetId`, `expectedSourceFingerprint`, an authored `rationale` and `candidates`.
The latter is an explicit locator input, useful for generated/public fixtures;
it is never a provider receipt, verified word boundary or inspected-frame claim.
An empty candidate list is valid. Current evidence must match the supported
moment recipe and current review representation. The complete canonical row,
source fingerprint and actual provider/model/prompt/schema/policy/representation
identities enter cache identity; replacing a row under the same moment identity
invalidates selection. Selection can explicitly consider labeling-hidden media
without changing eligibility for paid analysis.

Candidates use the existing moment schema: `core`, `payoff`, optional `setup` and
`context`, `allowedRange`, `dialogueBoundaries` in source milliseconds, plus
`score`, `reason`, `event`, `role`, evidence and uncertainty. Core must contain
payoff; allowed range contains every supplied span and must fit the exact source
duration. Inverted or out-of-media input is refused without swapping or guessing.
The shortest supplied complete setup/core/payoff envelope is selected, expanded
through overlapping timed utterances. It is never the first N source seconds by
default. Evidence remains uncertain, including names, species, transcripts and
OCR; selection does not verify those claims.

For example, after discovering a current moment identity, a minimal input is:

```json
{
  "version": 1,
  "decisionOwner": "human",
  "goal": "Compress travel while preserving the arrival",
  "sequences": [{
    "sequenceId": "arrival",
    "goal": "Approach, reveal, arrival",
    "pacing": {
      "preset": "rapidMovement",
      "preferredMinSeconds": 0.5,
      "preferredMaxSeconds": 2.0,
      "longReviewSeconds": 8.0,
      "preferredUpperSeconds": 30.0,
      "preferredLongHolds": 2,
      "allowMontageFragments": true
    },
    "sources": [{
      "kind": "current",
      "assetId": "DISCOVERED_ASSET_ID",
      "expectedMomentIdentity": "DISCOVERED_MOMENT_IDENTITY",
      "candidateIds": []
    }],
    "budgetSeconds": 30.0,
    "placeContext": null
  }],
  "budgetSeconds": 30.0,
  "minimumScore": 0.5,
  "repetitionPenalty": 0.5,
  "coverageBonus": 0.1
}
```

An empty `candidateIds` filter considers all candidates in that current artifact;
it cannot create candidates when the source has none.

### Pacing and constraints

A pacing profile supplies `preset` (`natural`, `rapidMovement`, or `custom`),
`preferredMinSeconds`, `preferredMaxSeconds`, `longReviewSeconds`,
`preferredUpperSeconds`, `preferredLongHolds` and `allowMontageFragments`.
Natural defaults are 3–8 seconds, review above 8, occasional longer holds and a
30-second preferred upper bound. These are preferences, not mandatory padding or
an instruction to chop useful actions or speech. To request brisk travel, use
0.5–2 seconds and deliberately allow movement fragments. Only movement without
known speech, outside explicit hero choices, is cropped; the interval centers on
the supplied payoff and keeps it whole. A payoff longer than the preference is
retained and surfaced for review. Other complete beats can naturally take 9–20
seconds. Roles alone do not certify a long take's value: content rationale,
uncertainty, alternatives and actual assembled listening remain necessary.

The selector compares marginal value across the chapter, penalizes repeated
events/sources and passive waiting, and exposes early/middle/late coverage.
Coverage uses the **authored source-list order and within-source payoff position**,
not corrected capture chronology or proof of continuous inspection. Disjoint
useful events from one source can survive; overlapping/adjacent optional ranges
are not counted as manufactured variety. Stable candidate identity breaks ties
deterministically. There is no minimum-runtime filler. Budgets constrain optional
choices; explicit required choices can exceed a soft budget and expose the
tradeoff. `hardBudget` makes that conflict blocking.

`include`, heroes and hard counts/relationships express explicit identity choices.
A relationship specifies `before`, `after`, `keepTogether`, `hard` and `rationale`.
Hard keep-together groups are considered together, with requested before/after
order. A role count can be global or scoped by `sequenceId`, with minimum and
maximum, `hard` and rationale. Impossible requirements, exclusions, cycles,
missing context and incompatible budgets are reported. Complete beats are not
silently shortened to satisfy them. A selection may therefore be shorter than its
budget or be an invalid but inspectable draft.

Authoritative confirmed source allow/exclude ranges and active retained human
feedback choices always apply. `retainedChoices` only routes a note to a sequence,
role and activity; it does not create a second retention lock. Existing assignments
supply routing when available. Missing routing is blocking. To release a retained
cut, explicitly supersede its feedback decision. Stable surviving cuts preserve
existing explicit audio; no J/L edit is silently converted to linked sound.

### Establishing a place and reviewing a draft

For a `majorPlaceChange`, supply `placeContext` with kind `visual`, `spoken`, or
`humanSupplied`, a description, `decisionOwner: "human"`, and the actual
`establishingCandidate` key. That source moment is required first in the sequence.
The input identifies context the author expects the **actual picture or spoken
sound** to supply. It does not render a caption/card from a description or infer a
location from GPS/OCR. Check the assembled clip to establish that a viewer really
knows where the new sequence is.

Reports recompute requested source-range seconds from actual revision operations,
including later feedback trims and reorders. They include earned time/gaps,
per-sequence activity and coverage, role duration distributions, long-take review
items, alternatives and conflicts. These are not measured encoded movie seconds.
Every assignment gives its sequence purpose, content reason, declared neighbor
context and evidence limitations. Long or repetitive material needs a reviewable
reason; a high score is insufficient. Human endpoint refinement does not turn
model core/utterance decimals into canonical accepted source-range constraints.
Unknown newly added cuts need a selection assignment so they cannot evade budgets.

Ordinary/feedback forks inherit immutable intent. A new selection can explicitly
replace it; feedback targets then flag `editorial_intent_changed` even when cut
picture/audio stayed the same. Preview identity includes the metadata fingerprint.
Portable operation payload version 3 carries selection metadata; plain and explicit
audio plans retain versions 1 and 2. Selection cache rows are local derived results,
not synchronized human facts. Prior revisions and their intent stay unchanged.

Before a whole-film render, inspect source endpoints with sound, render representative
travel, sustained action and dialogue sequences, and record a separate human
listening/pacing review. Generated tests verify mechanics only. Neighbor-context
review and short A/B comparisons are tracked separately under issues 184 and 185.


## Chronology-safe edit-plan placement

This section freezes the edit-plan chronology contract. The
owning implementation is `rcdvlog-core`; the CLI and desktop adapters only pass
typed inputs through and render core results.

### Scope

This is an edit-plan ordering capability, not a second clock solver and not a
story-beat store. The existing chronology facts, solver, corrected trip times,
source-range constraints, immutable revision guards, selection metadata, audio
event model, preview validation and export validation remain authoritative.
Ordinary insertion never infers or applies a new clock correction, never uses a
filename as a substitute for a known source time, and never treats retrieval
rank as story order.

### Frozen JSON contract

An edit-plan patch keeps version `1`. `EditPlanPatch::Insert.anchor` is
optional:

```json
{
  "version": 1,
  "name": "safe additions",
  "edits": [
    {
      "kind": "insert",
      "operation": {
        "operationId": "terminal",
        "position": 0,
        "assetId": "ASSET",
        "sourceInSeconds": 1.0,
        "sourceOutSeconds": 2.0,
        "rationale": "Terminal footage before boarding"
      }
    }
  ]
}
```

Omitted `anchor` is automatic chronological placement. `before`, `after` and
`end` remain explicit checked placements; an explicit anchor never opts out of
chronology or protected-group validation. Incoming insertion order is not
story order: automatic additions are sorted by corrected trip time and then by
stable operation/source identity before placement. Unknown source time,
contradictory chronology, and genuinely tied placements that cannot be ordered
from an exact source range fail with a bounded conflict.

A revision may carry one immutable policy, supplied as `storyPolicy` by
complete-plan creation or a sparse patch (when omitted, exact compatible
base policy is inherited):

```json
{
  "version": 1,
  "openingTeasers": [{
    "operationId": "teaser",
    "assetId": "ASSET",
    "sourceInSeconds": 0.0,
    "sourceOutSeconds": 3.0,
    "rationale": "Opening teaser intentionally previews the payoff",
    "decision": { "actor": "human", "rationale": "Reviewed opening choice" },
    "chronologyDependency": ""
  }],
  "protectedGroups": [{
    "groupId": "setup-payoff",
    "cuts": [{
      "operationId": "setup",
      "assetId": "ASSET_A",
      "sourceInSeconds": 0.0,
      "sourceOutSeconds": 2.0
    }, {
      "operationId": "payoff",
      "assetId": "ASSET_B",
      "sourceInSeconds": 4.0,
      "sourceOutSeconds": 7.0
    }],
    "rationale": "Setup and payoff are one reviewed unit",
    "decision": { "actor": "human", "rationale": "Keep as a contiguous unit" },
    "chronologyDependency": ""
  }],
  "exceptions": [{
    "exceptionId": "boarding-to-terminal-flashback",
    "rationale": "Deliberately return to the terminal after showing boarding",
    "decision": { "actor": "agent", "rationale": "Scoped editorial rearrangement" },
    "scope": {
      "cuts": [{
        "operationId": "terminal",
        "assetId": "TERMINAL",
        "sourceInSeconds": 1.0,
        "sourceOutSeconds": 2.0
      }, {
        "operationId": "boarding",
        "assetId": "BOARDING",
        "sourceInSeconds": 0.0,
        "sourceOutSeconds": 2.0
      }],
      "edges": [{
        "earlierOperationId": "boarding",
        "laterOperationId": "terminal"
      }],
      "chronologyDependency": ""
    }
  }]
}
```

`decision.actor` is exactly `agent` or `human`. Agent editorial decisions are
never rewritten as human approval. Empty `chronologyDependency` fields are
stamped by the core from the exact policy cut/source identities. A supplied
token must still match current chronology facts. Every exception is bound to
exact operation identities, exact source ranges, exact adjacent sequence edges
and the chronology dependency token; there is no project-wide unsafe switch, no
chapter-label exemption and no rationale-string parsing.

`EditPlanRevision.storyPolicy` is returned by `edit-plan get` and `edit-plan
list`. Creation and patch results also include it in the immutable revision.

### Placement and validation semantics

- Opening teaser cuts are an exact prefix and are excluded from the main journey
  ordering. Automatic additions are placed after the teaser prefix.
- Protected groups are exact, ordered, contiguous cut/source identities. An
  automatic addition may be placed before or after a group, never silently
  inside it. Moving, trimming, replacing, omitting or splitting a protected cut
  invalidates the exact group scope and is refused unless a new reviewed policy
  is supplied.
- Automatic additions are sorted as a batch by corrected source chronology,
  independent of incoming JSON enumeration order. They are placed into the
  retained main journey only at a slot that satisfies both neighbors and does
  not split a protected group or an existing exact exception edge. Multiple safe
  slots return a bounded conflict requiring an explicit checked anchor. If chronology places a new cut
  inside a protected group's time envelope, the bounded result is a conflict.
- Explicit anchors remain checked placements. A backward explicit join is an
  atomic validation failure unless an exact scoped exception covers the
  resulting adjacent edge.
- Existing overlapping source ranges and repeated footage remain legal but are
  explicit: opening-teaser reuse is declared in policy; an ambiguous ordinary
  tie requires an exact scoped decision rather than an inferred order.
- Missing chronology is not guessed. Automatic insertion of an asset without a
  solved trip time fails. So does a genuine tie between different assets.
  Unresolved or tied cross-source order blocks sparse edits and finishing until
  an exact scoped decision is recorded, including when an explicit anchor is
  supplied. Full-plan drafts remain inspectable with these blocking findings.
  Forward excerpts of the same take are comparable without a wall clock.
- Group exemptions cover only internal edges of that exact group. The edge
  between two separate groups remains chronological unless explicitly excepted.
- Solver contradictions are blocking validation errors. Changed chronology
  facts invalidate exact policy dependencies even when the retained main order
  is still monotonic.
- Audio timeline retiming still follows retained picture movement; automatic
  insertion uses the same sparse-patch audio retiming path as explicit
  insertion.

### Write-path and compatibility contract

`create_edit_plan_from_base` (with an optional explicit policy), every sparse
structural patch, activation, preview, OTIO export and delivery validation use
the same core chronology/group validator. Invalid immutable drafts remain
readable, but they cannot be activated, previewed, exported or delivered.

Schema migration 66 adds nullable `edit_plan_revisions.story_policy_json`.
Legacy revisions load with `storyPolicy: null`; migration never treats an old
backward edit as approved. New creation operations carry `storyPolicy` in a
payload version 5 while versions 1-4 remain replayable. Baseline export/import
and operation replay preserve the policy. SQLite remains canonical.

Preview, inspection and delivery constraint identities include current chronology
facts for the selected picture sources, even when a plan has no exceptional
policy. A later correction invalidates an old verification receipt even if the
new times remain monotonic. Sparse placement and whole-plan creation also recheck
those dependencies while holding the write transaction.

The desktop typed API accepts the same optional policy when creating a revision;
no ordering logic is duplicated in TypeScript or Tauri. There is no
interactive policy editor in the desktop workbench.

Replacing an existing nonempty policy requires `revisionDecision` with an explicit
`actor` and nonempty `rationale` on the replacement policy. This also applies to
an empty replacement that retires a teaser, protected group or exception. The
change decision is stored on the new immutable revision; omitting `storyPolicy`
continues to inherit the previous contract without changing it.


## Confirmed human source ranges

`confirmed_source_ranges` is the single canonical projection of explicit human
source-range decisions. The immutable project operation stream owns their
history and reconstructs them during baseline/sync replay. Local inspection
receipts, moment candidates, reviewer suggestions, notes and exported JSON are
not competing stores or implicit confirmations.

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan ranges list --asset <asset-id> --offset 0 --limit 100
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan ranges confirm --input /absolute/human-range.json
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan ranges revoke --asset <asset-id> --constraint <constraint-id> --expected-revision 1 --rationale "Human withdrew this exclusion after playback"
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan suggest-trim --input /absolute/trim.json
```

A confirmation asserts that the caller is recording an actual human decision:

```json
{
  "assetId": "source-asset-id",
  "expectedRevision": 0,
  "expectedSourceFingerprint": "indexed-source-fingerprint",
  "decision": "exclude",
  "media": "picture",
  "decisionOwner": "human",
  "sourceInSeconds": 0.0,
  "sourceOutSeconds": 0.5,
  "rationale": "Human confirmed the lens-covered opening",
  "inspectedEvidenceId": "inspect_<64-hex-digest>",
  "overridesEvidenceIds": []
}
```

`media` is `picture`, `audio`, or `both`; it is required. Excluding covered
picture does not require throwing away useful dialogue under B-roll. Audio
exclusions can instead mark unusable sound. Both scopes are explicit and are
checked independently. A silent cut has no source audio to constrain; linked
and independent source sound honor applicable audio bounds.

`decision` is `allow` or `exclude`. The latter subtracts ranges from the former.
Without active allow records, the known source duration is provisionally
available except for explicit exclusions; that default does not certify media
as clean. Multiple allow records form a union. Exclusions always take precedence,
including over later allow records. Revoke/update an exclusion explicitly to
withdraw it. A range cannot be moved to another source/project by reusing its
ID. To update one, supply its `constraintId` and current source-aggregate
`expectedRevision`. Omit the ID to create a new human record.

Bounds are finite source seconds, inclusive-in/exclusive-out, positive and
within the indexed source duration. Changed source identities/durations require
human reconciliation; their constraints are retained, never silently ignored.
Decisions are not inferred from sparse frames or model decimals. Inspection
sampling limits remain attached to the decision, and no field claims that a
human-entered timestamp or timed transcript segment is a verified source frame
or word boundary.

`inspectedEvidenceId` is optional for a manual decision made outside inspection.
When supplied, it must identify current completed evidence for this exact asset,
with intact files, the current complete representation hash, mapping, orientation,
recipe, tools and explicitly included transcript. The confirmed interval must
fit the inspected window. Verification never repairs/rebuilds what the human
saw; stale or damaged evidence reports `stale_evidence` and requires explicit
inspection and review again. Portable provenance records identity, mapping,
locator/proposal/inspection purpose, audio/transcript inclusion, window and sampling limits without local file paths. A peer honors the human
fact without needing the originating node's derived files.

`overridesEvidenceIds` records up to 64 explicit evidence/flag IDs that the
human overruled, for example a map, dark scene, hand or close-up falsely flagged
as unusable. Keep the reason in `rationale` (required, at most 4096 bytes).
An override is not a rotation/removal mutation and does not revoke another hard
exclusion. Model suggestions cannot use a `model` decision owner or invoke this
mutation through the inspection interface.

List results expose `schemaReady`, a single source-aggregate revision and paged
records including revoked decisions. Records/revision share one SQLite read
snapshot. Pages allow 1–200 records, with at most 1024 human records per source.
Separate `feasiblePictureRanges`, `feasibleAudioRanges`, `pictureConflicts` and
`audioConflicts` show the current effect. Read commands do not migrate or generate
media. Explicit writes require the expected revision even for the initial zero;
concurrent stale writes fail with `source_ranges_stale_revision`, appending no
local operation. Revocation is an immutable operation and keeps its prior history.

Ordinary edit-plan drafts remain inspectable even when invalid. Validation emits
blocking `edit_plan_confirmed_range_violation` findings for conflicting picture
or selected source-audio ranges. Activation/rendering enforce them. Rendering
records a constraint revision key for its used sources before validation, then
checks that key under a short SQLite write transaction before atomic publication.
A decision made while media renders prevents publishing that attempt and retains
the earlier completed file. It does not hold a SQLite writer lock during the
expensive media work. Existing previews may describe earlier decisions; consumers
must validate their revision/current constraints before presenting them as current.

### Advisory trim request

```json
{
  "assetId": "source-asset-id",
  "inspectedEvidenceId": "inspect_<64-hex-digest>",
  "media": "audio",
  "sourceInSeconds": 1.2,
  "sourceOutSeconds": 1.8,
  "preferUtterances": true
}
```

The proposal must fit the current inspected window. Clean ranges intersect that
window with authoritative constraints for the requested `picture` or `audio`
media. Normal suggestions intersect the proposal with those ranges. With
`preferUtterances`, accepted segment timings may expand a proposal only when
the entire utterance is inside one clean range and the inspected window. An
utterance crossing an exclusion or missing handles stays a conflict, never a
reason to extend into an excluded interval. Missing current speech timing is
reported explicitly. This command runs no transcription or forced alignment.

Results include the inspection ID, constraint revision, clean/proposed ranges,
conflicts and limitations. They never create or activate a plan or confirm a
constraint. Segment-level timing is not word alignment, waveform low energy is
not verified silence, and a sparse contact sheet is not continuous inspection.
Listen and inspect the actual mapped endpoint frames before accepting a speech
trim. Human listening/pacing review remains separate from generated mechanics
tests.


## Chronology evidence export

`rcdvlog --catalog /abs/catalog.sqlite3 chronology evidence --project PROJECT_ID [--offset 0] [--limit 100] [--facts-revision TOKEN]` emits a bounded JSON page from the local catalog. The limit is 1–500. Each response includes a `factsRevision`, non-asset chronology context, stable asset ids, capture sequence/counter membership, timestamp candidate observations, solved rows (including applied evidence), contradictions, and `nextOffset`.

Pass the first page's `factsRevision` on every subsequent request. If any chronology fact changed, the command returns `chronology.facts_changed`; restart from offset zero and use the new token. Reads use the stored catalog only and do not probe media. New probes store format and video-stream `creation_time`, `recording_start_time`, and `recording_end_time` tag observations, plus compact filename timestamp observations. Each carries exact origin, original text, parsed value, timezone claim, parse status, and source-known semantics. A generic `creation_time` tag is not treated as a recording start or end. Filename timestamps remain timezone-free wall-clock observations with unknown start/end meaning. Legacy assets return `timestampCandidates: null` to state that candidate provenance was not retained; a new probe that found none returns an empty array.

The revision mismatch has a stable CLI error code, `chronology.facts_changed`. The command exits nonzero, leaves stdout empty, and writes one JSON error envelope to stderr with `ok: false`, `command: "chronology.evidence"`, and `error.code`/`error.message`. The message retains the `chronology.facts_changed` marker for human-readable diagnostics.

Read-only chronology commands support catalogs through schema 25. In those catalogs timestamp-candidate provenance was not stored, so the core chronology loader supplies `null` and the evidence page reports `timestampCandidates: null`; it does not infer an empty candidate list. Read-only opens never apply migrations. To add provenance support to the catalog, run an explicit writable CLI command such as `rcdvlog project list --catalog PATH` (which performs the normal catalog upgrade on writable open), then rerun `rcdvlog catalog inspect --catalog PATH` and confirm the schema is current. The project catalog remains unchanged by chronology reads.

`candidateComparisons` summarizes observable disagreements. Wall-clock values without an explicit timezone are never compared as UTC instants. Start/end duration comparisons are included only when both stored candidates explicitly carry those semantics and both parse to timezone-aware instants; `durationDeltaSeconds` is the observed interval minus clip duration, and `durationExplainsDifference` says whether it is within one second. Explanations keep camera-clock evidence, display timezone, and physical trip location separate; no observation applies a correction. Camera clock correction remains a separate explicit chronology operation.

Timestamp candidate records are stored with the immutable project asset operation and included in portable baselines, so offline replicas retain the evidence. Filesystem modification time is not substituted for camera timestamp evidence.

