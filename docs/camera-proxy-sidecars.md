# Camera supplied proxy sidecars

Some camera cards include a same-name `.LRF` companion beside an original clip.
VlogCutter can discover and register these files without changing either media
file. Association uses a recorded original location for that asset on this node.
The preferred original may be organized elsewhere under a different filename;
an alternate card copy must already be recorded as verified for the same asset
(for example by ingest or relink). Discovery does not infer locations by searching
for a reused counter or filename on another card.

```sh
rcdvlog camera-proxy discover --project PROJECT_ID --card /path/to/card
rcdvlog camera-proxy register --asset ASSET_ID --card /path/to/card --sidecar /path/to/card/DCIM/CLIP0001.LRF
rcdvlog camera-proxy status --asset ASSET_ID
rcdvlog camera-proxy frame-pack --asset ASSET_ID
```

Discovery and registration share the same validation. They canonicalize the
card root and require the original and sidecar to stay under it, in the same
exact directory/run, with the same stem. They repeat the original's asset
fingerprint check (and compare full files when the primary is available), so a card reusing a previously recorded pathname is rejected.
An unrecorded copy is never an association root. Primary locations can be
verified observationally; alternate locations must already have a verified
same-asset record. Discovery does not modify the catalog, cache or media.

The accepted mapping has zero-origin constant-rate presentation timestamps,
one source second per sidecar second, and equal frame rates or integer frame-rate
reductions from 2 through 8 (including 60 → 30 and 60000/1001 → 30000/1001).
Every video packet presentation timestamp must match its nominal CFR timestamp within 2 ms;
retiming, nonzero starts and variable frame rates are unsupported. Video frame
counts must cover the declared duration, and source/sidecar durations must agree
within one sidecar frame plus 2 ms. Mapping concerns source-relative media time;
it never changes capture timestamps, chronology or human orientation decisions.

Registration additionally requires H.264 video, AAC audio, lower resolution,
matching aspect/display rotation, full sidecar decode, and five corresponding
32×18 RGB samples across the clip. Each sample's mean absolute channel error
must be at most 16/255. This conservative content check can reject unusual
camera processing; it is bounded sample evidence, not proof of every frame or
certification of future edit ranges. Full-file hashes before and after validation
reject files changed during verification. Both files remain immutable.

SQLite stores the original absolute and card-relative location, original asset
and full-file fingerprints, capture-sequence identity when recorded, sidecar
path, full BLAKE3 fingerprint, byte length, codec, dimensions, duration,
verification time, validator, sample evidence and explicit rate/time mapping.
Registration holds the existing per-asset proxy claim and atomically replaces
only the camera representation row after verification succeeds. A failed
replacement preserves the previous row. Discovery reports `candidate`,
`registered`, `missing`, `association_mismatch`, `unsupported`,
`unsupported_rate_mapping`, `unsupported_timing`, `content_mismatch`, `mismatch`
or `truncated_or_unreadable`, with a reason. Status reports a missing or changed
registered sidecar as `stale`.

Camera sidecars remain distinct from generated recipes and finishing originals.
They do not set `assets.proxy_path`, count as RawV3, or change generated-proxy,
transcription or preparation readiness. `frame-pack` explicitly samples the
registered camera representation, returning a standard frame-pack manifest with
`representation: camera.h264-lrf.verified-v1`, source-relative `timestampsMs`, and
a `sourceFingerprint` binding both verified bytes and mapping provenance. The
command is local, read-only and makes no provider call. The existing generated
analysis pipeline continues to use its own representation policy. Legacy
registrations remain reviewable but require explicit re-registration before
frame-pack use; migration never invents time or content verification evidence.

Review selects a generated proxy first, then an available original, then a
registered sidecar whose full-file fingerprint still matches. Media delivery
uses the existing bounded byte-range stream. Originals may be offline after
registration; the sidecar and its saved mapping suffice for review and the
explicit frame-pack command. A changed or missing sidecar is never selected.
