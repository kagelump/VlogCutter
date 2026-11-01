# Chronology-safe edit-plan placement

This document freezes the edit-plan chronology contract for issue #218. The
owning implementation is `rcdvlog-core`; the CLI and desktop adapters only pass
typed inputs through and render core results.

## Scope

This is an edit-plan ordering capability, not a second clock solver and not a
story-beat store. The existing chronology facts, solver, corrected trip times,
source-range constraints, immutable revision guards, selection metadata, audio
event model, preview validation and export validation remain authoritative.
Ordinary insertion never infers or applies a new clock correction, never uses a
filename as a substitute for a known source time, and never treats retrieval
rank as story order.

## Frozen JSON contract

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
    "exceptionId": "terminal-before-boarding",
    "rationale": "Opening terminal shot deliberately precedes boarding",
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
        "earlierOperationId": "terminal",
        "laterOperationId": "boarding"
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

## Placement and validation semantics

- Opening teaser cuts are an exact prefix and are excluded from the main journey
  ordering. Automatic additions are placed after the teaser prefix.
- Protected groups are exact, ordered, contiguous cut/source identities. An
  automatic addition may be placed before or after a group, never silently
  inside it. Moving, trimming, replacing, omitting or splitting a protected cut
  invalidates the exact group scope and is refused unless a new reviewed policy
  is supplied.
- Automatic additions are sorted as a batch by corrected source chronology,
  independent of incoming JSON enumeration order. They are placed into the
  retained main journey only at a slot that introduces no uncovered backward
  edge and does not split a protected group. If chronology places a new cut
  inside a protected group's time envelope, the bounded result is a conflict.
- Explicit anchors remain checked placements. A backward explicit join is an
  atomic validation failure unless an exact scoped exception covers the
  resulting adjacent edge.
- Existing overlapping source ranges and repeated footage remain legal but are
  explicit: opening-teaser reuse is declared in policy; an ambiguous ordinary
  tie requires an exact scoped decision rather than an inferred order.
- Missing chronology is not guessed. Automatic insertion of an asset without a
  solved trip time fails. So does a genuine tie between different assets.
  Existing inspectable plans may carry an unresolved warning, but a new
  automatic placement cannot use that uncertainty as permission.
- Solver contradictions are blocking validation errors. Changed chronology
  facts invalidate exact policy dependencies even when the retained main order
  is still monotonic.
- Audio timeline retiming still follows retained picture movement; automatic
  insertion uses the same sparse-patch audio retiming path as explicit
  insertion.

## Write-path and compatibility contract

`create_edit_plan_from_base` (with an optional explicit policy), every sparse
structural patch, activation, preview, OTIO export and delivery validation use
the same core chronology/group validator. Invalid immutable drafts remain
readable, but they cannot be activated, previewed, exported or delivered.

Schema migration 66 adds nullable `edit_plan_revisions.story_policy_json`.
Legacy revisions load with `storyPolicy: null`; migration never treats an old
backward edit as approved. New creation operations carry `storyPolicy` in a
payload version 5 while versions 1-4 remain replayable. Baseline export/import
and operation replay preserve the policy. SQLite remains canonical.
