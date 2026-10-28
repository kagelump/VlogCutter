# Revision-bound edit-plan feedback

Review feedback is a bounded, portable operation stream attached to immutable
edit-plan revisions. It stores editorial observations and explicit responses
to those observations; it is not a general story-beat or script store. The
fact kinds remain separate: an immutable **note** records an observation,
a **proposal** suggests a typed edit, and a **human decision** accepts or declines
that proposal (or retains an existing cut). A decision in the `applied` state
records a host receipt of a prior human acceptance. A target **mapping** is an
explicit human fact; it does not rewrite the note or proposal.

All seven CLI commands require an existing explicit catalog. `record`,
`propose`, `decide`, `map`, and `apply` are writes; `get` and `report` open the
catalog read-only and do not migrate it. JSON inputs are bounded to 64 KiB and
reject unknown fields.

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

## Preview evidence and playback locators

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

## Proposal, decision and draft apply

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

## Retaining an existing cut

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
