# Rough-cut editing and review readiness

Build the pinned supported CLI from this checkout (`pnpm build:cli`). Editing,
inspection, review, rendering and YouTube delivery use that executable and one
catalog. Before editing, run:

```sh
rcdvlog --catalog /absolute/project/catalog.sqlite3 doctor --workflow rough-cut
rcdvlog --catalog /absolute/project/catalog.sqlite3 doctor --workflow delivery
```

These observational checks require the workflow commands, supported media tools
and a current compatible catalog. They do not migrate, render, load upload
credentials, contact a provider or upload a movie. Delivery additionally requires
the upload/history command surfaces. Check authorization separately with
`youtube auth status`; transfer still requires an explicit `youtube upload`.
No speech models are required merely to edit existing evidence.

Schema 63 incorporates the upload ledger after the editing migrations. The exact
historical upload-branch schema 49 is recognized as upgradeable. Its original
migration identity is retained in `catalog_migration_reconciliations`; upgrading
installs the merged history atomically and preserves upload records. Unrecognized
divergent histories are refused. Back up before explicitly opening a catalog
writable to upgrade. Schema 64 adds local review-coverage evidence. Schema 65 adds the immutable patch-lineage projection, with version-4 creation operations for lossless sync/replay.

## Count source use and find unused material

`edit-plan coverage report --input /absolute/coverage.json` accepts:

```json
{
  "revisionId": "CANDIDATE_REVISION",
  "baseRevisionId": "BASE_REVISION",
  "targetPercent": 50,
  "inventoryAssetIds": null,
  "groups": [{"name": "arrival", "assetIds": ["SOURCE_A", "SOURCE_B"]}]
}
```

The denominator defaults to every project asset, including removed/offline
sources. If a catalog includes non-footage bookkeeping rows, supply the explicitly
declared full source inventory as `inventoryAssetIds`. The report fingerprints
that scope and reports the project row count separately. It refuses duplicate,
unknown or omitted pictured identities. Reuse the same inventory across revisions;
do not drop excluded/unavailable footage to raise the percentage.

Reports distinguish unique pictured sources, repeated use, audio-only sources,
added/retained/dropped sources, per-group usage, target distance and requested
runtime delta. These are planned source seconds, not measured movie duration.
An approximate usage goal does not authorize runtime filler or weaken constraints.

```sh
rcdvlog --catalog /absolute/catalog.sqlite3 edit-plan coverage unused \
  --revision REVISION --offset 0 --limit 20
```

Optional `--group-input` is one `{name,assetIds}` object; `--include-excluded`
includes explicitly marked unusable candidates for diagnosis. The bounded queue
reuses descriptions, labels, existing moment locators, source handles and confirmed
allowed ranges. Connective-word matches rank retrieval hints only. Stale evidence
and offline sources remain explicit; this command never generates or uploads media.
Source-inclusion coverage differs from chapter spread and audiovisual review coverage.

## Apply a sparse immutable revision

```sh
rcdvlog --catalog /absolute/catalog.sqlite3 edit-plan patch \
  --project PROJECT --expected-base BASE --expected-active none \
  --input /absolute/patch.json
```

```json
{
  "version": 1,
  "name": "Quicker arrival",
  "edits": [{
    "kind": "insert",
    "anchor": {"kind": "before", "operationId": "CUT_ARRIVAL"},
    "operation": {
      "operationId": "NEW_TRANSFER_VIEW",
      "position": 0,
      "assetId": "SOURCE_TRANSFER",
      "sourceInSeconds": 1,
      "sourceOutSeconds": 2,
      "rationale": "Distinct preparation detail",
      "audio": {"mode": "silent"}
    }
  }]
}
```

Supported kinds are `trim`, `replace`, `insert`, `reorder`, `omit`, `audio` and
`split`. Anchors are `before`/`after` with `operationId`, or `end`. Changes use
explicit source seconds; split supplies `sourceSplitSeconds`, `newOperationId`
and rationale. Input array order defines batch evaluation. A target may appear
once per batch; missing/duplicate/reused identities and removed anchors are refused.
Inserted `position` is recomputed from its explicit anchor.

The command validates the complete candidate before committing one normal inactive
revision. Failure leaves no partial revision. Existing human constraints and the
expected active revision remain authoritative. Unchanged IDs survive; inserted,
split and replacement lineage is retained in the creation operation and result.
Named alternatives use the existing [comparison commands](edit-plan-comparisons.md).

Inherited independent audio follows its owning cut, preserving source sync through
prefix insertions and trims. An explicit `audio` or replacement payload uses the
new timeline's absolute placement. Splitting an explicit sound-bearing cut retains
one independent sound event; its second picture fragment is explicitly silent.
Insufficient handles, negative placements and real sync/overlap conflicts are
rejected rather than clipping dialogue automatically. Numerical adjacency below
one nanosecond is treated as touching, not a new audible overlap.

## Record decisions and review evidence accurately

New feedback decisions and comparison preferences may supply:

```json
{"provenance":{"actor":"agent","authority":"delegatedEditing","confidence":"provisional"}}
```

An actual confirmed user choice uses `human`, `explicitHumanChoice`, `confirmed`.
Actor, authority and confidence must agree. Agent decisions may apply provisional
proposals, but cannot retain cuts as human locks or supersede retained human
choices. Legacy explicit `decisionOwner: "human"` requests remain supported.
Historical facts without provenance remain unchanged and have unknown detailed
authorship; do not infer it from a rationale. An explicit human superseding
decision can correct ambiguous legacy retention without rewriting history.

`latestAttempt` is an ordering fact. Fresh verification may establish
`currentForRequest: true` for an older exact preview when its movie, media,
constraints and plan still match. This permits feedback on what was actually
viewed while preserving stale activation and real dependency invalidation.

Use [review coverage](edit-plan-review-coverage.md) for separately recorded picture,
listening, methods, intervals and remaining review questions. Sparse frames,
transcripts, technical decode and energy samples never fill a human listening gap.
Use [bounded inspection](edit-plan-inspection.md) for endpoints and explicit
fallback precision, and [delivery bundles](edit-plan-delivery.md) for verified local
movie/mapping/coverage/contact artifacts.

## Render progress and audio audit planning

Preview progress emits explicit `phase` events for source identification, assembly,
audio mix/mux, output decode/validation, input verification and publication.
Long phases update their durable stage and emit a heartbeat at cancellation
observation boundaries, at most once per five seconds. Remaining time is unknown
without phase evidence. Clip ETA covers encoding only; its last update never
claims zero whole-render work remains. Cancellation and failure keep prior outputs.

`edit-plan audio-audit plan --input /absolute/sample-plan.json` accepts
`{artifactId,sampleSeconds,maxSamples}` for a completed bounded excerpt. It returns
a ready `AudioIntegrityRequest` under `request`, derived from measured joins and
declared independent-audio boundaries. Limits are 0.02–1 second and 1–32 samples;
windows use integer 48 kHz ticks, fit the movie and expose unchecked ranges and
deferred boundaries. The subsequent audit rechecks bytes. Energy is technical
evidence, not intelligibility or editorial listening acceptance.

## Acceptance

The generated-media CLI test `rough_cut_readiness_cli` exercises one catalog through
preflight, source discovery, sparse insertion, coverage delta, retained independent
audio, repeated previews, historical feedback, provisional agent decisions, review
coverage, bounded audio audit planning/execution and bundle reuse. OAuth/upload
protocol tests use fake endpoints. Native editor import, actual Google consent,
remote processing/playback and human editorial listening remain separate checks.
