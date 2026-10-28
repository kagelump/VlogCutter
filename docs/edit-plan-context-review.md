# Whole-cut context excerpts

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

## Assemble evidence for a reviewer

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
separate explicit command. These commands use the bounded #124 local inspection
tools and constraints. They do not invoke a cloud agent or upload media.
