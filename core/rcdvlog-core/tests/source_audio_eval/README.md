# Source-audio evaluation assets

Vendored evaluation assets for the source-audio MVP
(`docs/tasks/tv-review/SOURCE_AUDIO_MVP_PLAN.md`, step 1) plus deterministic
public synthetic audio for CI. This directory is the repo-side eval/test home
for the source-audio recognizer: it sits under `core/rcdvlog-core/tests/`
because the frozen policy and expected metrics are fixtures for the core
parity tests, next to the existing `tests/fixtures/` convention, and the
synthetic media here are the public fixtures consumed by later CLI
process-level tests. `tests/source_audio_eval.rs` locks the frozen hashes,
the privacy rules and fixture determinism.

Layout:

| Path | What |
| --- | --- |
| `policies/pooled-recognition-policy-v3-anchor-aware.json` | frozen v3 acceptance policy, byte-identical vendor |
| `expected/m1-small-set-anchor-aware-results-v2.sanitized.json` | sanitized expected metrics (rankings, acceptance outcomes, identity fields) with provenance |
| `synthetic/generate-synthetic-fixtures.mjs` | fixed-seed synthetic audio generator |
| `synthetic/synthetic-ci-v1.json` | generated fixture manifest with expected offsets and SHA-256 digests |
| `synthetic/movie.wav`, `synthetic/recording-continuous.wav` | generated public fixtures (about 580 KiB total) |
| `runner/run-source-audio-eval.mjs` | reusable evaluation runner (index -> match -> collect) |

## Vendored artifact provenance

Artifact paths below are relative to `SOURCE_AUDIO_EVAL_ROOT` (private; see
the next section). SHA-256 identities are the plan's frozen table:

| Artifact | SHA-256 | Status here |
| --- | --- | --- |
| `policies/pooled-recognition-policy-v3-anchor-aware.json` | `75b006faea7287bd31ce000b9fd2439290f41fdcf3a6ee8e2b22f6eea19efd4e` | vendored byte-identical at `policies/` |
| `reproduction/m1-small-set-anchor-aware-results-v2.json` | `41dcc1ee05923f1897b8a1f902606ca5cdaad9edddd0349c59c031f5d23eabb7` | sanitized extraction at `expected/`; provenance note inside records this source file and digest |
| `manifests/m1-small-set-v1.json` | `80ae9adc10138f33f72b168c57f79ba5d1f9a1dc803a581700ad36daeabb6b44` | not vendored (contains private media paths); identity recorded |
| `preservation/m1-window-pooling-and-anchor-identity.patch` | `afc1774af82c0e2197a6e138fe6e127a100ca1ca424eb1236ebe5b91d35593d0` | not vendored; its request shape (`pooledOffsetWindows`, `pooledOffsetModeLimit`) is the base for the runner-manifest request |

Verify the vendored policy with:

```sh
sha256sum core/rcdvlog-core/tests/source_audio_eval/policies/pooled-recognition-policy-v3-anchor-aware.json
```

The frozen policy is copied with exact bytes. It embeds seven opaque
calibration capture IDs (hash-like identifiers, no paths or media names);
byte-identity is mandated by the frozen SHA-256 above, and the mapping back to
captures lives only in the private evaluation root.

The sanitized metrics extract the source file's top-level identity fields,
summary rows and per-window case rows verbatim and in source order. The only
sanitization: each case row's `sessionId` (which doubles as a private media
filename stem) is replaced by `caseIndex`, its zero-based position in the
source file's `cases` array; the source file SHA-256 restores the exact
mapping for private replay. The source results file contains no media paths.

Frozen outcomes the extraction records (development evidence, small set):

* 17/17 positive windows top-ranked within 100 ms at each of density50 and
  density100; worst observed error 40 ms.
* Density100, 10-second v3 acceptance: 5/6 positives (the moderate composite
  is rejected with 31 independent anchors against the 33 floor), 0/13
  negatives (0/7 calibration + 0/6 audit).
* Density50 abstains on v3 acceptance (`duration_or_density_not_calibrated`);
  0/13 negatives accepted at both densities.

## Private replay prerequisite

Reproducing the frozen results requires the private evaluation root and its
local media: set `SOURCE_AUDIO_EVAL_ROOT` to the eval-2 directory (policy,
manifests, requests, inputs and reproduction outputs). Nothing from that tree
is committed here except the byte-identical policy and the sanitized metrics
extraction above. Private media, indexes, binaries and correspondence dumps
stay local; private replay is a local-only activity and its outputs belong
under `$SOURCE_AUDIO_EVAL_ROOT/mvp/**`, never in this repository.

## Synthetic CI fixtures

`synthetic/` contains original synthetic audio only (no RC6-derived content):

* `movie.wav` — 12.0 s synthetic movie soundtrack: seeded chord events of
  square-wave oscillators plus a seeded noise floor, 11025 Hz mono 16-bit PCM;
* `recording-continuous.wav` — 15.0 s: 3.0 s of seeded room noise, then the
  movie heard from its start (plus seeded mixing noise), 11025 Hz mono PCM;
* `recording-seek.wav` — 28.0 s: the movie heard at two distinct known delays
  (from its start at recording time 2.0 s, then — after a 1.0 s seek — from its
  start again at recording time 15.0 s), 11025 Hz mono PCM. Segment-assembly
  expectations for it live in the manifest's `integrationCases[]` (kept out of
  `cases[]` because each playback run has its own expected offset).

Regenerate (two runs must produce byte-identical outputs):

```sh
node core/rcdvlog-core/tests/source_audio_eval/synthetic/generate-synthetic-fixtures.mjs --out <dir>
```

The generator uses fixed xorshift32 seeds and only IEEE-754 basic arithmetic
(no `Math.sin`/`pow`/... which are implementation-defined), so outputs are
byte-identical across runs and machines. Current digests
(SHA-256, also recorded inside `synthetic-ci-v1.json`):

| File | SHA-256 |
| --- | --- |
| `movie.wav` | `8870e7be64c3257a3630b1348f5511df33e0a6e374eeb6f55974002d15d0e86a` |
| `recording-continuous.wav` | `8531cedc7e3524c5d6691f527788479884a790070cbe544fb32cf4f4175214dd` |
| `recording-seek.wav` | `75435e3f0d69e0302ad39477e7e6f2d7b25955fd6d3c486da085785ff8e8da22` |
| `synthetic-ci-v1.json` | `df6775861643a4fb3bd9722bcc120ecf8d5e75c04547d8ee75c8b5d71259c7d0` |

### Offset sign convention (explicit)

The frozen design defines:

```
movieElapsedSeconds = recordingSecondsWithinEpoch + offsetSeconds
```

The synthetic recording plays the movie from its start at recording time
3.0 s, so `0.0 = 3.0 + offsetSeconds`, i.e. the expected offset for every
requested window is **`offsetSeconds = -3.0`** (negative: the movie position
is earlier than the recording position). A recording window at [3, 13) s with
this offset maps to movie [0, 10) s. The manifest repeats this derivation in
`cases[0].expected` and states the convention string verbatim; the runner
rejects a manifest with any other convention string.

## Evaluation runner

Explicit process contract (all four arguments required):

```sh
node core/rcdvlog-core/tests/source_audio_eval/runner/run-source-audio-eval.mjs \
  --binary <built rcdvlog> \
  --manifest <manifest.json> \
  --media-root <dir> \
  --output <receipt.json> \
  [--work-dir <dir>] [--dry-run]
```

* `--binary` — built `rcdvlog` binary.
* `--manifest` — evaluation manifest (shape below).
* `--media-root` — directory under which the manifest's media paths resolve;
  all media paths must be relative and stay under this root.
* `--output` — run receipt JSON path.
* `--work-dir` — scratch directory for the index and per-case request/report
  JSON (default `<output>.work`); execute mode requires it not to exist.
* `--dry-run` — print and validate the plan without executing anything. The
  binary does not need to exist in dry-run mode, so the runner is usable
  before the imported CLI lands.

The runner executes, per manifest:

1. `tv-review source-audio-landmark-index --input <index-input.json> --movie <movie> --output <index.json>`
2. per case: `tv-review source-audio-landmark-match --index <index.json> --input <request.json> --recording <recording>`
3. each match's stdout envelope is saved as the case's report JSON and the
   receipt records its SHA-256 plus per-window outcomes.

Exit codes: 0 success/valid plan, 1 validation or execution failure, 2 usage.
Each step has a 900 s timeout. Run the synthetic dry-run like this:

```sh
node core/rcdvlog-core/tests/source_audio_eval/runner/run-source-audio-eval.mjs \
  --binary target/release/rcdvlog \
  --manifest core/rcdvlog-core/tests/source_audio_eval/synthetic/synthetic-ci-v1.json \
  --media-root core/rcdvlog-core/tests/source_audio_eval/synthetic \
  --output /tmp/source-audio-eval/receipt.json --dry-run
```

### Manifest shape

The manifest is what the generator writes (`synthetic-ci-v1.json` is the
complete example) and the runner validates strictly:

* `schemaVersion: 1`, `fixtureSet`, and `offsetConvention` equal to the
  frozen convention string above.
* `movie`: `path` (relative to `--media-root`), `sha256`, `sampleRate`,
  `sampleCount`, `durationSeconds`, and `clock` — the `SyncMovieClock`
  (`movieDigest`, `timeBaseNumerator`, `timeBaseDenominator`, `originPts`,
  `durationPts`).
* `indexRequest`: `LandmarkIndexInput` (`movie` equal to `movie.clock`,
  `fftSize` 512/1024, `hashesPerSecond` 20..=100, `fanout` 1..=3).
* `cases[]`: `caseId`, `recording` (`path`, `sha256`, `sampleRate`,
  `firstSample`, `lastSample`, `captureClockId`), `request` and `expected`.
* `integrationCases[]` (optional, ignored by the runner): segment-assembly
  evidence for recordings without one case-level offset — `caseId`,
  `recording`, `pooledOffsetWindows`, `pooledOffsetModeLimit` and `expected`
  with per-window `expectedOffsetSeconds`, per-segment
  `supportedSecondsWithinEpoch` and `unknownTransitionSecondsWithinEpoch`
  (consumed by `core/rcdvlog-core/src/source_audio_segments_tests.rs`).
* `request` is a full match request per the preserved CLI request schema
  (`LandmarkMatchInput`): `captureClockId`, `policy`, `searchPolicy`,
  `diagnosticOffsetEvidencePerTile`, `pooledOffsetWindows`
  (`{startSeconds, durationSeconds}` with durations 10/20/30 seconds),
  `pooledOffsetModeLimit` (1..=64) and `sync` (`SyncRequest`). Unknown keys
  are rejected in dry-run, matching the CLI's `deny_unknown_fields`.
* `expected`: `offsetSeconds`, `offsetToleranceSeconds`, optional
  `derivation`, and `windows[]` mirroring `pooledOffsetWindows` with a
  `expectedOffsetSeconds` per window.

Digests in the manifest are SHA-256 of the generated file bytes;
`targetDigest` reuses the movie digest (the synthetic set has no review
target, matching existing core fixture convention), and the matcher reports
its own BLAKE3 `recordingDigest` in match output.

## Expected output JSON shapes

Match step stdout (the report JSON the runner collects) is the CLI envelope:

```json
{
  "ok": true,
  "command": "tv-review.source-audio-landmark-match",
  "data": {
    "schemaVersion": 1,
    "movieDigest": "...",
    "indexDigest": "...",
    "recordingDigest": "<BLAKE3 of the recording file>",
    "pooledOffsetEvidence": {
      "complete": true,
      "offsetCellScanComplete": true,
      "modeLimit": 8,
      "windows": [
        {
          "startSeconds": 3,
          "durationSeconds": 10,
          "offsetCellsScanned": 0,
          "offsetCellsPooledForWindow": 0,
          "offsetCellsDropped": 0,
          "bestDistinctCompetitor": {},
          "modes": [
            {
              "offsetBin": 0,
              "offsetSeconds": 0.0,
              "independentAnchors": 0,
              "sharedQueryAnchorsWithBest": 0
            }
          ]
        }
      ]
    },
    "search": { "complete": true, "rawOffsetEvidenceCount": 0 },
    "alignment": {},
    "mapping": {}
  }
}
```

Failures return `{ "ok": false, "error": { "code": "...", "message": "..." } }`.
`modes` is ranked best-first; `modes[0].offsetSeconds` is the top-mode offset
compared against `expected` (compare within `offsetToleranceSeconds`).

The runner's `--output` receipt is:

```json
{
  "schemaVersion": 1,
  "runner": "run-source-audio-eval.mjs",
  "mode": "dry-run | execute",
  "status": "plan valid | complete | failed",
  "contract": { "binaryPath": "...", "manifestPath": "...", "mediaRoot": "...", "outputPath": "...", "workDir": "..." },
  "binaryPresent": true,
  "manifestSha256": "...",
  "offsetConvention": "movieElapsedSeconds = recordingSecondsWithinEpoch + offsetSeconds",
  "media": [ { "path": "movie.wav", "sha256": "...", "bytes": 0, "declaredSha256": "..." } ],
  "indexStep": { "argv": [], "status": "planned | ok | failed", "reportPath": "...", "reportSha256": "...", "indexDigest": "..." },
  "cases": [
    {
      "caseId": "continuous-movie-at-recording-3s",
      "status": "planned | ok | failed | skipped",
      "command": [],
      "reportPath": "...",
      "reportSha256": "...",
      "expectedOffsetSeconds": -3.0,
      "expectedOffsetToleranceSeconds": 0.1,
      "completeness": { "searchComplete": true, "offsetCellScanComplete": true },
      "windows": [
        {
          "startSeconds": 3,
          "durationSeconds": 10,
          "expectedOffsetSeconds": -3.0,
          "topModeOffsetSeconds": -3.0,
          "withinTolerance": true
        }
      ]
    }
  ]
}
```

In dry-run mode steps are `planned`, no reports exist and `topModeOffsetSeconds`
is null. `withinTolerance` is null when no mode was reported.

## Verification

```sh
cargo test -p rcdvlog-core --test source_audio_eval
```

covers: the frozen policy SHA-256, the sanitized metrics provenance and
privacy rules, the manifest/media digest and WAV-header consistency, the
offset sign convention, and (when `node` is available) that the generator
reproduces the committed fixture bytes and that the runner passes `--dry-run`.
