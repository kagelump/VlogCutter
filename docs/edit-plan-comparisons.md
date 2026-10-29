# Short local edit comparisons

Short comparisons package two to four named variants from one project. Each
variant selects one to twelve contiguous whole cuts from an immutable revision;
each rendered excerpt is limited to 60 seconds. The renderer reuses the normal
preview, source identity, audio, validation, cancellation and artifact owners.
The comparison creates a local HTML player that loads variants sequentially in
one video element and can repeat the sequence. Native video controls keep audio
available. It has no autoplay or external assets.

## Create and inspect

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

## Record a human preference

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
