# Bounded source-window inspection

Use the same Rust `Catalog::inspect_source_window` primitive from the CLI,
desktop adapter, and a future bounded reviewer. It is local media work: it
never calls a model, transcribes, chooses a fallback, accepts a trim, marks a
defect, changes rotation or removes footage.

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan inspect --input /absolute/inspection.json
```

```json
{
  "assetId": "source-asset-id",
  "representation": "original",
  "purpose": "proposedBoundary",
  "sourceInSeconds": 1.0,
  "sourceOutSeconds": 2.0,
  "beforeSeconds": 0.25,
  "afterSeconds": 0.25,
  "sampleCount": 8,
  "sourceTimesSeconds": [1.25],
  "maxWidth": 768,
  "includeAudio": true,
  "includeTranscript": false
}
```

The candidate range is inclusive-in/exclusive-out. Handles expand its inspected
context; unavailable handles fail instead of silently shrinking. Limit the
whole window including handles to 60 seconds, total requested frames to 64,
and the maximum image dimension to 160–1280 pixels. JSON input is at most
64 KiB. Images preserve aspect ratio without upscaling. `sampleCount` distributes
requests across distinct decoded frames in the window. Additional
`sourceTimesSeconds` select the nearest available frame inside the window.
Requested times remain in the request; frame evidence reports the actual PTS.

Select `original`, `generatedProxy`, or `cameraProxy` explicitly. Offline or
incompatible selected media fails. Generated proxies must pass the shared
current-v3 analysis-source validation; legacy/unmapped camera sidecars fail.
Generated proxies have seconds correspondence, not a measured original-frame
map. Camera sidecars carry their verified nominal reduction/mapping. Exact
original-frame evidence requires the original. A denser request cannot recover
discarded frames. `availableFrameCount`, measured minimum/maximum available
frame intervals, returned distinct frames, and maximum unsampled gap expose
these limits; no sparse pack certifies intervening frames.

The result contains `inspection` and `reused`. An inspection binds the complete
selected-representation BLAKE3, indexed source identity, mapping/provenance,
effective orientation, request, media-tool identities and recipe. Frame evidence
includes an ID, integer `representationPts`, rational `mapping.timeBaseNumerator`
and `timeBaseDenominator`, source seconds, zero-based excerpt seconds, and an
image path/hash/length. The representation origin is explicit. Original PTS
are measured; source seconds mapped from proxies carry their weaker precision.
Video and container origins must coincide on the representation time base;
nonzero shared origins map back to source zero. A differing or unknown video
origin fails until an explicit compatible timeline mapping is available.
Rotation uses raw decoded frames with autorotation disabled, then applies the
catalog's effective rotation once. Inspection never infers or stores rotation.

To inspect exactly a previously decoded representation frame, use
`exactRepresentationPts: [12345]` and its
`expectedRepresentationFingerprint`, with `sampleCount: 0`. These are actual
PTS ticks, not frame numbers or guessed model decimals. Unknown PTS or frames
outside the window fail. A fingerprint mismatch reports `stale_evidence`.
Do not equate exact proxy PTS with exact source frames. Proposed cut endpoints
still need the source-frame/audio-sample checks and listening appropriate to
their intended use.

Sound has an independent opt-in. `includeAudio: false` never extracts audio or
reads a transcript. When true, the selected representation supplies stereo
48 kHz PCM WAV context and 20 ms waveform bins with peak/RMS and a low-energy
cue below −50 dBFS RMS. Source bounds round inward to complete 48 kHz decoded
samples; each bin reports excerpt timing. Low energy is neither verified
silence nor speech/quality evidence. A missing audio stream is `noAudioStream`;
a known leading stream gap is retained on the source clock and reported as
`completeWithLeadingGap` with `streamSourceInSeconds` and `leadingGapSeconds`;
failed or truncated extraction is an error, never successful silence or a
complete playable excerpt. No other representation supplies missing sound.

`includeTranscript: true` additionally requires audio opt-in. Only current
accepted segment text matching the selected generated proxy's fingerprint and
the supported local transcript recipe is reused. Original/camera inspections
have no implicitly borrowed transcript. Utterances retain their full source
segment bounds, clipped excerpt intersection and `clippedByWindow`. Word
alignment is unavailable and is stated explicitly. No targeted transcription or
alignment runs as a side effect. Segment boundaries and waveform cues cannot
establish complete words; listen before accepting speech trims.

`purpose` records `locator`, `proposedBoundary`, or `inspectedBoundary` intent.
All results say what was inspected and what remains uncertain. They never become
an accepted human constraint or confirmed covered/dark/unusable interval by
being cached. Maps, meaningful dark scenes, hands and close-ups require context
and human judgment. Confirmed constraints and bounded provider navigation are
separate consumers of this foundation, tracked by #124 and #176.

Completed receipts live in canonical SQLite `source_inspections`; files live
under the catalog's managed `cache/source-inspections`. Reuse requires identical
identity JSON and verified artifact contents. Range/handles, sampling, sound,
current accepted transcript, representation bytes, mapping, orientation, recipe
or tool changes invalidate reuse. Damaged/missing files rebuild in a unique
attempt directory, leaving earlier evidence intact. A persistent per-evidence
lock and the shared host-heavy lease protect ownership; cancellation is checked
during hashing, media children, waveform work, verification and before durable
publication. Failed/cancelled attempts never acquire a completed receipt. The
current CLI/desktop adapters do not expose a cancellation control; the domain
callback is available to an owning run controller. Local receipts are derived,
node-local evidence and do not add human facts to the sync operation stream.

Generated numbered-frame/tone tests verify mechanics and mappings. They do not
establish natural speech, visual defect classification, editorial timing or
story quality. Record a separate listening/playback review for those judgments.
