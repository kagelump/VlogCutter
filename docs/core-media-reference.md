# Core data, media and analysis reference

Relocated verbatim from the repository-root `AGENTS.md` section 9 ("Core data and media behavior") so the root file can stay a short index (issue #260). This file holds the full text, recipes and measurements.

## Assets and patches

`Asset` is defined canonically in `core/rcdvlog-core/src/lib.rs` and mirrored in `apps/desktop/src/types.ts`. Availability is computed on read:

- `originalAvailable`: at least one node-local location exists and its sampled
  fingerprint has been verified against the asset identity.
- `proxyAvailable`: stored proxy path currently exists.
- `available`: original or proxy is available.
- Thumbnail path/quality are returned only when the stored file exists.

The Rust/Tauri `AssetPatch` is a full annotation replacement: notes, labels, removed, and rotation are all required. React's `patchFor(asset)` deliberately sends the full current state. The CLI offers patch-like optional flags by fetching the current asset, merging changes, and then sending the full core patch. Do not change one side without preserving this behavior.

## Ingest and identity

Ingest recursively scans without following symlinks and accepts these extensions case-insensitively:

```text
mp4 mov m4v mts m2ts avi
```

Identity uses a sampled BLAKE3 fingerprint based on file size plus up to 1 MiB from the beginning, middle, and end. The unique database constraint is `(project_id, fingerprint)`. Re-ingest is idempotent within a project. Originals stay at their existing paths.

ffprobe supplies duration, frame rate, dimensions, codec, tags, and timestamp candidates. ExifTool currently supplements camera make/model; absence or failure should degrade to other metadata rather than prevent every ingest.

## Chronology

Assets are ordered deterministically by:

```text
best_time IS NULL, best_time, filename, id
```

Raw camera timestamp text is preserved separately. `best_time` is normalized RFC 3339 UTC when parseable. A clock event applies a fixed offset to one camera from a lexical filename boundary onward. Recalculation chooses the latest applicable boundary and never mutates source metadata.

The current compact story ribbon spaces clips by ordinal position, not duration or elapsed wall-clock distance. This is intentional: relative ordering and context are more important than absolute time span. Default column width is 68 px, `[`/`]` zoom, and `C` centers the selected clip. Do not revert it to a fit-all duration plot.

## Proxies

Proxy generation uses one shared explicit encoder policy. `auto` is the safe
default; `software` never initializes hardware. Linux `auto` first runs a bounded
synthetic FFmpeg probe and only then may use the VAAPI full-clip
decode/scale/H.264-encode path. Device-file presence or an encoder listing alone
never marks hardware usable. Capability JSON distinguishes `device_unavailable`,
`device_permission_denied`, `ffmpeg_encoder_unavailable`,
`initialization_failed`, and `usable`; `proxy probe --policy auto|software`
exposes it. `proxy provenance --asset <id>` exposes durable attempt/fallback
evidence, and `proxy history` exposes encoder/worker-scoped rates.

On macOS up to four local workers run concurrently. Each first tries
VideoToolbox hardware decode and H.264 encode with the CPU scaler retained for
materially better downscale quality. The hardware settings are equivalent to:

- `-hwaccel videotoolbox` with H.264 `h264_videotoolbox` High profile.
- VideoToolbox constant quality 36 and `power_efficient=1`.
- Scale down to at most 1280 px wide while preserving aspect ratio.
- AAC 128 kb/s and `+faststart`.

If VideoToolbox is unavailable, busy, or rejects an input, the Mac retries the
existing software recipe automatically. On Linux the VAAPI recipe initializes
one render node for hard decode, `-filter_hw_device`, `scale_vaapi`, and
`h264_vaapi` at QP 23. A hardware process or validation failure removes only the
unique staging output and retries software. The Ubuntu companion server reads
`RCDVLOG_PROXY_ENCODER_POLICY` (default `auto`) and persists actual encoder,
attempt, fallback, and validation evidence in server job details. The standard
software recipe remains:

- Override the input display rotation to 0 and disable autorotation, preserving raw encoded orientation without copying the camera display matrix.
- Scale down to at most 1280 px wide while preserving aspect ratio.
- H.264 `libx264`, `veryfast`, CRF 23, `yuv420p`.
- AAC 128 kb/s and `+faststart`.

FFmpeg proxy processes emit output-timestamp progress once per second. Convert
that duration fraction into partial source-byte work so active clips reduce the
ETA before they finish. Partial work is display/ETA state only and must not
update persisted throughput; only a successful completed clip contributes its
full bytes and worker duration. Reset partial work if VideoToolbox fails before
starting the software fallback, and retain completion-only behavior when FFmpeg
does not provide a usable timestamp.

Only the original is accepted as proxy input. If the original is offline, proxy generation fails explicitly. Generated candidates are written to unique staging files and validated for H.264, at most 1280 px wide, `yuv420p`, zero display rotation, source-time duration correspondence, source-audio presence as AAC, and successful video+audio decode before atomic commit. Each transaction writes `proxy_generation_provenance` (recipe, actual encoder, device, policy, timing, ordered failures, fallback flag, validation, timestamp) in canonical SQLite; a failed transaction never changes the registered `assets.proxy_path`. Version-2 proxies are obsolete because they may retain the source display matrix; a later “missing” run regenerates them and skips only valid current-version paths that still exist.

Proxy and thumbnail writers share cross-process claims in the managed `cache/.representation-locks` directory. Claims use the canonical cache root, asset ID, and representation kind; lock files persist after release and must never be unlinked. SQLite remains canonical. A busy single-item CLI writer exits with JSON code `representation_busy`; batch writers count contention as skipped work without recording a failure or throughput sample. Proxy downloads claim before requesting content and stream into unique staging paths, so a stale writer cannot overwrite a later transaction.

## Thumbnails

Only quality 1 is implemented. It is a JPEG at 20% of clip duration, at most 480 px wide, generated to a unique staging file and atomically renamed to `q1.v2.jpg`. Input preference is original, then available proxy. Qualities 2–10 are reserved and must continue to fail clearly until intentionally implemented.

Q1 thumbnails and analysis-selected Q10 representative thumbnails use the same cross-process thumbnail claim as desktop and sync work. See the proxy section above for busy-result and persistent-lockfile behavior; keep the existing output names and recipes unchanged.

## Transcription

Local speech-to-text ships. It runs entirely offline with no API key and never
touches the network. Design and measurements:
spec (`4c5eb2f:docs/superpowers/specs/2026-08-03-stt-vlm-clip-analysis-design.md`),
round-1 spike (`4c5eb2f:docs/spikes/2026-08-03-stt-analysis-spikes.md`).

- **Audio comes from the proxy, never the original.** The original may be
  offline on the NAS, and transcription must work whenever proxies exist. A clip
  with no proxy fails explicitly rather than falling back.
- **Recipe key**: `stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1`. A
  transcript is identified by that key plus the sampled BLAKE3 fingerprint of the
  proxy it came from, so regenerating a proxy makes its transcript stale.
- **Batch key**: `stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1-w1`,
  one worker. Peak RSS is 1.15–1.38 GB and concurrent transcription alongside the
  four-worker VideoToolbox proxy pipeline has never been measured.
- **CPU-only, deliberately.** Do not enable `whisper-rs`'s `metal` feature: ggml
  gates `MTLResidencySetDescriptor` on the SDK rather than the deployment target,
  so a Metal build emits a macOS 15 symbol and will not launch on the documented
  OS floor. The dependency is declared `default-features = false`.
- **Probe audio before extracting PCM.** Transcription reads the proxy and
  calls the reviewed `probe_source_audio_present` FFprobe helper first. A valid
  video with no audio stream completes as `no_audio` with zero VAD/ASR/model
  work and is reused like any other terminal transcript. A probe or extraction
  failure stays `failed` and retryable; corrupt media is never reclassified as
  silence.
- **The VAD gate is a correctness requirement, not an optimization.** Given
  silence, whisper invents fluent speech at high confidence. A clip with no
  detected speech is stored as `no_speech` with reason `no_detected_speech` and
  is never handed to any model. `no_speech` is a success and must never be
  conflated with `failed`, or every silent clip is retried forever. Detected
  speech followed by empty or unusable provider output is
  `no_accepted_speech` with reason `empty_provider_output`, not silence.
  Quality-gate rejections remain `rejected`; exact proxy fingerprints,
  detected-speech duration, decode-window count, raw provider segment count,
  mean token probability and repetition ratio stay attached to the outcome.
  Token probability is optional and stored as SQL NULL when a candidate has no
  comparable measurement; missing evidence is rejected explicitly rather than
  invented as confidence 0 or 1. Provider confidence and average log
  probability are different native metrics and must not be relabelled as a
  mean token probability.
- **Legacy compatibility is explicit.** Rows written before audio probing and
  provider-output distinctions existed keep status `no_speech` but migrate with
  reason `legacy_no_speech_unverified`. Preparation marks those rows
  review-required because they cannot prove verified silence. Transcript
  coverage reports `no_audio` and `no_accepted_speech` separately.
- **Windows are 30 seconds.** whisper bills a full window per decode regardless
  of content, so speech regions are packed into as few windows as possible. Do
  not reintroduce the PRD's 8–25 second chunking: measured, it ran 30% slower on
  39% less audio.
- **Do not use `no_speech_probability()`** as a quality gate. Measured median
  0.000 for good and bad transcripts alike. The local gate that works is
  repetition at `>= 0.3` or a known token-probability mean below `0.20`.
  Non-empty candidate output with any missing probability evidence is rejected
  as `missing_probability_evidence`; malformed provided values are rejected as
  `invalid_probability_evidence`. Unknown evidence must not dilute a known bad
  mean into a pass.
- **A batch never downloads a model.** It fails with an actionable error naming
  the file, its size, and where it belongs. Installation is a separate, explicit
  user action: global settings and CLI `models install` share
  `install_speech_models_with_progress`. Preserve this split; a 574 MB
  transfer must never begin as a side effect of pressing Analyze.
- Models live in a shared application-support directory
  (`<data dir>/com.rcd.vlogcutter/models/`) so one copy serves every project. The
  older per-project `<project folder>/cache/models/` location is still read when
  it holds the model, so existing installs are not re-downloaded.
- CLI `models status` reads identity, integrity, readiness and managed location
  without catalog writes, downloads or engine initialization. CLI `transcribe
  batch` validates an exact project-owned selection, reuses current non-failed
  transcripts, and opens one engine for pending clips. Progress includes per-asset
  outcomes and a one-worker estimate; cancellation preserves completed work and
  failures remain retryable. Existing single-clip and whole-project commands stay
  compatible. See `docs/agent-cli.md`.

**Measured end to end**, release build, M3 MacBook Air (fanless), one worker,
273-clip Osaka/Expo project, 129.1 minutes of footage:

| Measure | Value |
| --- | --- |
| Wall time | **31.5 min** |
| Clips with speech | 131 |
| Clips gated as `no_speech` | 135 (49%) |
| Rejected by the hallucination gate | 7 (5 repetition, 2 low token probability) |
| Failures | 0 |
| Windows decoded | 155 |
| Speech audio | 15.9 min |
| Cost per window | ~12.2 s |
| Transcript segments stored | 380 |

Three of these correct round-1's projections and should be trusted over them:

1. **The corpus needs ~155 windows, not the ~20 round 1 projected.** That
   projection divided total speech by 30 seconds, but a window cannot span two
   clips, so the floor is one window per speech-bearing clip. 126 of 131 speech
   clips used exactly one window. Per-window cost was right; the count was off
   7x, and with it the "~3.5 minutes for the whole project" figure.
2. **The VAD gate skips 49% of clips, not 64%.** whisper.cpp's built-in VAD at
   default parameters gates less aggressively than the tuned Silero ONNX harness
   round 1 used. That costs decode time and is the safer direction, but the
   thresholds are worth revisiting.
3. **12% of speech clips get a language tag outside the supported en/ja/zh set**
   — 12 Korean, 2 Russian, 1 Polish, 1 Spanish. Round 1 called Korean a
   hallucination marker on this corpus. A World Expo plausibly contains all of
   them, so this is unverified either way, but STT language tags are currently
   **not** domain-validated against a closed vocabulary the way the design
   requires for model output.

## Clip analysis

Cloud vision analysis is opt-in and runs from either the CLI or the desktop. It
reads proxies, never originals,
and sends a deterministic pack of JPEG frames plus an accepted speech transcript
when one is current. The OpenRouter key resolves from `OPENROUTER_API_KEY` or
`~/.openrouter.key`; it is never stored in a project catalog.

- **Frame recipe**:
  `framepack.2fps.skip400ms.max40.win3-8.sharp-bright-motion.dhash8-d5.v1`.
  Candidates begin 400 ms after the start and omit the final 400 ms. Selection
  is deterministic and combines sharpness, brightness, motion, and dHash
  diversity into 3–8 frames.
- **Prompt version**: `vlm.observe.v2`. Every response passes a bounded JSON
  schema and domain validation. The representative thumbnail must name one of
  the supplied frame timestamps.
- **Providers**: Qwen is the default. A process-wide circuit breaker switches
  the batch to Gemini after repeated transport/provider failures. A
  domain-invalid default response is retried once with Gemini.
- **Ownership**: title, description, and AI labels are suggestions. Human notes
  are never touched; label consolidation remaps only labels recorded in the AI
  ownership side table and commits the resulting label changes as typed project
  operations. Title and description are human-editable through the dedicated
  `set_clip_summary` operation; they are deliberately not part of `AssetPatch`,
  whose four fields are a full-replacement contract shared with the CLI.
- **Staleness**: analysis is current only for the same frame recipe, prompt
  version, representation, and proxy fingerprint. Regenerating the proxy makes
  the observation stale.
- **Budget/concurrency**: the cost ceiling and worker count are parameters on
  `AnalysisRunOptions`, falling back to `RCDVLOG_VLM_MAX_COST_USD` and
  `RCDVLOG_VLM_WORKERS` (default 4) when unset. The legacy runtime ceiling is
  checked before each request against returned provider usage and is not yet
  cross-process safe. The durable admission/dispatch foundation is
  `analysis::accounting` plus migration 38: initialize its canonical authority
  before creating new runtime run/pilot rows, then atomically reserve the full
  provider-owned bound and commit the dispatch marker before transport. Provider
  wrappers and runtime/CLI wiring are follow-up work; do not claim concurrent
  runtime safety until they call this owner. Progress weights are the exact
  number of selected frames.
- **Cancellation**: batch progress callbacks return `ControlFlow`. Returning
  `Break` sets the same stop flag the budget ceiling uses, so workers stop
  claiming new clips without abandoning a request already paid for. Every stage
  commits per clip and skips current clips, so a cancelled run resumes.
- **Run accounting**: `analysis_runs` records one row per desktop run with its
  stage, counts, spend, and stop reason. The pre-run cost projection comes from
  this project's own observed cost per clip, falling back to
  `REFERENCE_COST_PER_CLIP_USD` until the first run. Like `analysis_labels`, the
  table is local and unsynced.

Within-clip moment discovery lives in `analysis/moments` and is exposed through
the thin `moments plan/discover/list/get` CLI adapter. It requires explicit
project/asset scope, provider, model, payload and spend ceiling, reuses current
proxies and accepted timed speech, and stores resumable steps and evidence in
SQLite. Reuse the shared representation claims and analysis-run accounting;
unknown usage blocks new paid requests until explicit recovery authorization.
Sparse coverage and candidate ranges remain suggestions requiring manual
review; discovery never changes inclusion, human annotations or chronology.

The `prepare preflight/run/evidence` CLI composes existing representation,
transcript, orientation, and moment evidence for an explicit project/asset
scope. Preflight and evidence require an existing explicit catalog and open it
read-only; only `prepare run` mutates, and paid moments work requires its
explicit provider/model/payload/budget policy. Pagination outcomes describe
only the current page, scope fingerprints are selectors rather than snapshots,
and sparse samples never certify arbitrary edit ranges. Full proxy replacement
invalidates transcript binding. Preparation preserves originals, human
annotations, orientation choices, and chronology; see `docs/agent-cli.md`.

Paid analysis is admitted only through a concrete `RunPolicy` (see
`docs/analysis-preflight.md`). The policy binds exact project-owned asset/source
identities, provider/model allowlist and permitted fallbacks, frames/audio/
transcript payload, explicitly approved project-vocabulary context, expected
cost, requested ceiling, prompt/schema/recipe/tool versions and readiness.
Authorization is a separate SQLite record from the bounded runtime pilot. An
explicit CLI/desktop batch invocation materializes that policy automatically,
records approval, runs the pilot, then executes the exact authorized scope; no
sample or budget approval broadens model/payload/scope. Frames-only requests
must not read or transmit transcripts; project vocabulary requires its own
explicitly approved context and stays frozen to that snapshot during a run. Pilot spend counts against
the same ceiling; unknown or interrupted usage is retained and fail-closed.
Any policy/source/schema/context change invalidates the approval and pilot,
including after restart. Provider diagnostics persist only stable categories,
never response bodies or raw error strings.

**Measured end to end**, 2026-08-04, M3 MacBook Air, four workers, 273-clip
Osaka/Expo project:

| Measure | Value |
| --- | --- |
| Main pending batch | **252 clips in 10 min 29 s** |
| Final current coverage | **273 / 273** |
| Failed or rejected | 0 |
| Transcript fused | 131 clips |
| Provider fallback | 1 clip (Qwen 429 to Gemini) |
| Stored analysis cost | **$0.02732851** |
| Negative controls | 12 / 12 passed; $0.000505 |
| Consolidated vocabulary | 6 labels; 149 asset mappings |

## Batch estimation

All long batch work should use `core/batch-estimator`, not an ad hoc ETA. Callers provide work weights, explicit worker count, optional persisted `RateHistory`, and successful per-worker elapsed observations. The estimator balances remaining weights across workers and reports the slowest shard. Failures advance item completion but do not contaminate throughput.

Current task keys are versioned. macOS proxy generation has its own history
because VideoToolbox throughput is not comparable to the server/software path:

```text
proxy.h264-720p.orientation-v3.macos-videotoolbox-w{1,2,3,4}-v2
proxy.h264-720p.orientation-v3.macos-software-w{1,2,3,4}-v2
proxy.h264-720p.orientation-v3.linux-vaapi-qp23-wN-v1
proxy.h264-720p.orientation-v3.linux-software-wN-v1
thumbnail.q1.orientation-v2
stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1-w1
vlm.observe-v2.framepack.2fps.skip400ms.max40.win3-8.sharp-bright-motion.dhash8-d5.v1-w{1,2,3,4}-v1
```

Change the task key when settings or concurrency change enough that old throughput is no longer comparable. Proxy and thumbnail weights are source bytes. macOS proxy batches use up to four workers, single-proxy calls and thumbnails use one, and the non-macOS/server proxy path remains one worker. A future cloud/VLM task must pass its real shard concurrency.

The server persists histories in its control catalog scoped to its stable node and task key. The default resource model is one shared local-heavy slot (proxy/transcode and full-hash cannot overlap) plus up to two overlapping lightweight workers. Durable progress must supply the exact remaining weights and actual worker count; only successful item observations update throughput.

## Host calibration and the shared local-heavy slot

`rcdvlog calibrate run` is an explicit, bounded, opt-in benchmark. It uses generated synthetic media by default; `--media` is required to authorize one representative file, whose bytes are read but whose path/filename are never persisted. Readiness, preflight, import, and normal analysis must never start a benchmark or download a model. Calibration results live in canonical SQLite and are reused only when host, build, media-tool executable/version, device/model, recipe, representation kind, and candidate configuration identities all match. Running/interrupted rows, failures, not-ready model conditions, changed tools, and explicit overrides remain uncertainty and must not be turned into a calibrated ETA or a completion promise.

One progress/eta range rule applies: a single timing sample widens the reported range rather than pretending a point estimate is precise. Speech calibration compares finite, non-overlapping, in-range detected regions against the baseline before adopting a faster VAD thread setting; it never assumes one thread is universally optimal. Proxy calibration uses the existing output validation/render/audio contract and records the actual selected encoder. Explicit encoder/worker/VAD/decode overrides are validated, carry explicit provenance, and use settings-qualified history keys when they differ from the defaults.

The accepted #53 resource model is one shared local-heavy slot across proxy/transcode, full-hash, speech, and future local VLM/analysis work. The server scheduler claims the union of heavy classes from one durable eligible-work queue (newest explicit heavy work first, intentional LIFO), and lightweight work may overlap under its own cap. Proxy, transcription, preparation, and calibration entrypoints take one advisory same-host/same-user filesystem lease; batch internals must not nested-acquire it. This lock coordinates processes on one host only and is not a distributed cluster scheduler. Existing per-asset representation claims remain separate integrity locks.

## YouTube publishing

Upload-only OAuth, persistent node-local credentials and resumable uploads are
implemented in `core/rcdvlog-core/src/youtube`, with a thin `youtube` CLI adapter.
Project schema 49 owns the local, unsynced upload ledger. Credentials stay outside
project catalogs (macOS Keychain or owner-only local file). Scope is exactly
`youtube.upload`; do not add read/management scopes for convenience. Desktop
loopback and Web client HTTPS callbacks share PKCE, state validation and token
storage. Web callbacks require an exactly registered HTTPS URI, a fixed loopback
port and a trusted proxy preserving Host; never enable Funnel or open the raw
listener on a non-loopback address. Auth status
is observational. Never silently recreate an expired/uncertain session or log
tokens, codes, verifiers, or resumable session URLs. See
[YouTube publishing](youtube-upload.md) for setup, recovery, JSON contracts
and outstanding real-Google/Keychain acceptance. Render scheduling is not
implemented; a caller explicitly invokes upload after a successful render.

## Edit planning and export

Edit-plan revisions are separate from labels and reversible removal. The basic
desktop action seeds a plan from visible, non-removed clips, while the core owns
ordered source ranges, validation, preview generation, and OTIO export. Do not
move rough-cut inclusion or source-range controls back into the labeling phase;
finish them in the dedicated edit-plan surface. Removal continues to control
labeling visibility only.
