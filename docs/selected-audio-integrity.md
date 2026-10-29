# Selected audio integrity audit

`edit-plan audio-audit` creates bounded local evidence about source audio presence and sampled energy across the registered original, the renderer-selected representation, and a rendered preview. It does not change a revision or make editorial decisions.

## Create and read evidence

Create an input file containing an `AudioIntegrityRequest`:

```json
{
  "revisionId": "editrev_…",
  "scope": {
    "firstOperationId": "cut_…",
    "lastOperationId": "cut_…"
  },
  "artifactId": "preview_…",
  "samples": [
    { "movieInSeconds": 0.25, "movieOutSeconds": 0.75 },
    { "movieInSeconds": 1.5, "movieOutSeconds": 2.0 }
  ]
}
```

Run and later retrieve the durable report with:

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite edit-plan audio-audit create --input /absolute/audio-audit.json
rcdvlog --catalog /absolute/project/catalog.sqlite edit-plan audio-audit get --evidence <evidence-id>
```

`create` emits progress events as JSON lines on stderr and the `AudioIntegrityResult` on stdout. `get` reads the historical report without checking whether its source or output files are still current. Both input and evidence content are bounded; input JSON is read with a 64 KiB cap. `get` is read-only.

For source-presence checks only, set `artifactId` to `null` and `samples` to `[]`. This never checks rendered audio. To audit a preview, the artifact must belong to the same revision and exact excerpt scope; sample movie times must fit its recorded decoded duration. The report states whether its source, tool, plan, and output identities still match.

Sample times start at zero in the excerpt. They are not full-revision or source
timestamps. Each result separately records the corresponding full-timeline
48 kHz mix interval. Listening copies are unnormalized float WAVs of the
expected master mix, expected selected-representation mix and decoded movie.
No master listening copy is invented for a source without an audio stream.

## Bounds and interpretation

An audit covers one contiguous 1–12-cut scope no longer than 60 seconds, at most 24 contributing sources, and at most 32 sampled intervals. Each interval is 20 ms to 1 second. The report records original and renderer-selected source presence/fingerprints, known picture-only cached segment contracts, audio mappings, sampled master/selected/rendered RMS and peak, playable WAV files, exceptions, and unchecked movie ranges. Missing or changed source/output identity yields partial or unchecked evidence rather than a claim that the source lacks audio.

The `original` (registered master) and selected representation are checked separately. The master is also checked against its registered ingest fingerprint; changed or unverified identity makes the evidence partial and prevents treating those bytes as the registered master mix. Sampled ingest identity is distinct from the full content hash recorded for this audit. A picture-only derivative that contains no audio is reported as a known silent derivative; it does not establish that the original/master is silent. `noSourceAudio` means the checked source has no audio stream. `deliberateSilence` can pass when the edit explicitly declares silence and the rendered sample has low energy. An unavailable original does not become a “no audio” conclusion.

For sampled energy, RMS at least `0.001` (about −60 dBFS) is meaningful sound energy, while RMS at most `0.0001` (about −80 dBFS) is low energy. Those thresholds can flag suspected missing or unexpected sound between master, selected representation, and render. They do not detect words, intelligibility, wind, clipping acceptability, sync, or mix quality. A stream can contain silence, and unsampled sections remain unchecked. Listen to the actual original/selected source and rendered movie for editorial acceptance; the generated WAV samples aid that review but do not replace it.

The command uses local media tools only. It does not run ASR, contact providers, inspect an entire corpus, activate plans, or record human feedback.
