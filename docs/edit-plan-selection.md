# Chapter-wide sequence selection

Schema 56 attaches optional versioned human intent and selection evidence to an
immutable edit-plan revision. The existing plan remains the only timeline.
Selection reads local candidate moments; it never calls a provider or transcribes
media. Editorial omission leaves capture chronology and labeling visibility alone.

## Supported commands

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

## Intent and evidence

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

## Pacing and constraints

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

## Establishing a place and reviewing a draft

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
