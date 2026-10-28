# Confirmed human source ranges

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

## Advisory trim request

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
