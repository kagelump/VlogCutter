# Edit-plan preview artifacts

`edit-plan preview` renders a local review movie without activating or changing
the addressed edit-plan revision. Each invocation creates a distinct attempt
with a unique `artifactId`; a generation number orders attempts with the same
request (revision and maximum height). Attempts move through `running` to a
terminal state: `complete`, `cancelled`, `failed`, or `interrupted`. Progress
updates carry the artifact ID and a monotonically increasing sequence number.
Consumers should retain the ID from the result or first progress event and
ignore older sequence numbers.

Use `edit-plan preview-list --revision ID` for a bounded page of summaries and
`edit-plan preview-status --artifact ID` for one full receipt. These are
observational reads: a present output path or completed receipt does not say
that current source media still matches. Pages contain at most 100 attempts;
status includes at most 100 cut outcomes and reports the total/truncation flag.
Before schema 53 is installed, listing returns `schemaReady: false` without
migrating the catalog.

`edit-plan preview-wait --artifact ID` polls until the attempt is terminal or
the observation timeout elapses (600 seconds by default, maximum 24 hours). A
timeout leaves the original render running. It does not cancel it, create a new
attempt, or replace any output. `edit-plan preview-cancel --artifact ID` is the
explicit cancellation request. It asks the process that owns the render to stop
cooperatively and reap its media children; the command returns the observed
status and does not itself wait for the render to finish. Terminal attempts
cannot be resumed. Start a new preview to make a new attempt. If the owning
process disappears, a running receipt with no live render claim is observed as
`interrupted`.

Each attempt owns a separate output directory under the project cache. A later
generation never overwrites a completed preview, so prior output paths remain
available for comparison until manually removed. There is no automatic receipt
or output cleanup. Per-cut outcome records describe render outcomes; segment
reuse across attempts is not implemented.

## What the receipt measures

The input identity records the recipe, revision and plan digest, requested
preview size, current human-range constraint key, selected source and
representation fingerprints, rotation and probed media clocks, and the
FFmpeg/FFprobe executable identities. Each output cut records the requested
source-clock in/out seconds and its actual decoded output-picture boundaries.
Picture timestamps come from decoded frame PTS; the renderer does not claim a
frame-by-frame correspondence back to the original source for the requested
decimal range.

The shared movie clock uses time base `1/240000`. The final picture interval
ends at the final decoded picture-frame boundary, derived from that frame's
duration. Container duration is reported separately and can extend beyond the
last picture interval because AAC priming or padding may add an audio tail. That
tail is not assigned a fictitious picture frame or added to the final cut's
picture duration.

## Explicit current-media verification

Pass `--verify` to `edit-plan preview-status` to request fresh verification; on
`preview-wait`, it requests verification after the attempt becomes terminal.
Verification checks that the owned output is still a regular file with the
recorded full-file BLAKE3 digest and byte length, and that its recorded media
decode completed. It rechecks current selected source/representation
fingerprints, media clocks, rotation, tool identities, recipe and plan identity;
it also compares the current source-range constraints and current plan
renderability. The response records these results and limitations separately.

Verification is evidence about bytes and technical media coverage at the time
of the check. It does not establish complete dialogue words, listening quality,
source-audio content parity, continuity, or editorial pacing. A stored receipt,
path existence, or green technical verification is not a human editorial
approval. These are derived, node-local catalog receipts, not synchronized
project decisions.
