# RCD VlogCutter agent CLI

`rcdvlog` is the non-interactive JSON control surface for desktop catalogs and the authenticated companion server. Local commands call the same Rust core as Tauri; remote commands use bounded documented HTTP operations rather than opening server databases.

## The CLI and the project-local agent workspace

This document describes the CLI as implemented today.

The accepted product architecture (PRD §6.3, §15, §25.1 item 18) pairs this CLI
with a project-local *agent workspace*. The CLI now reports its own build and
readiness, and can explicitly bind or verify the selected executable and
catalog for an existing project root. Issue #48 still owns materializing
project context such as generated `AGENTS.md` and `PROJECT.md` files.

The two layers have different jobs:

- **Workspace files** are the discovery, context, and bootstrap layer.
- **This CLI** is the authoritative transactional interface for reading and
  mutating canonical catalog and project state, with revision safety.

Workspace projections do not replace SQLite, which stays canonical. Directly
modifying a generated projection such as `PROJECT.md` or `edit-plan.json` is not
equivalent to changing VlogCutter state; changes flow back only through a
revision-safe CLI command or a future explicit, validated apply/import
operation. Human- and agent-authored files are different: the planned
`script.md` is a versioned input that may be edited directly.

Workspace binding does not generate or rewrite project instructions. Script
revisions and workspace materialization remain planned. Everything below is
implemented; anything marked planned is not.

## Build and catalog selection

```bash
rustup toolchain install 1.97.1 --component rustfmt --component clippy
cargo +1.97.1 build --locked -p rcdvlog-cli
target/debug/rcdvlog doctor
```

For a development binding, build from the intended checkout with that pinned
toolchain. Use the same `CARGO_TARGET_DIR` for the build and its resulting
binary; pass the binary's absolute path to `workspace refresh`. For an installed
selection, pass the absolute path of the installed binary. Do not build or
resolve another executable during workspace checks.

Catalog selection, in precedence order:

1. `--catalog /absolute/path/catalog.sqlite3`
2. `RCDVLOG_CATALOG`
3. The desktop app's persisted `lastCatalogPath`
4. The legacy desktop catalog under macOS Application Support

Ordinary catalog commands retain the selection precedence above. `doctor` is
observational and never falls back to the desktop catalog: without explicit
`--catalog` or `RCDVLOG_CATALOG` it reports `catalog_required`. Agent automation should pass
`--catalog <project folder>/catalog.sqlite3` explicitly. `workspace refresh`
requires that explicit catalog (or `RCDVLOG_CATALOG`) and never creates or
migrates it.

## Output contract

Success is one JSON value on stdout with exit code `0`:

```json
{
  "ok": true,
  "command": "project.list",
  "data": []
}
```

Failure is one JSON value on stderr with a nonzero exit code:

```json
{
  "ok": false,
  "command": "clips.get",
  "error": {
    "code": "not_found",
    "message": "clips.get failed: asset not found: asset_missing"
  }
}
```

Local desktop commands use `invalid_arguments`, `invalid_input`, `not_found`, `media_offline`, `media_tool_failed`, `representation_busy`, `stale_revision`, and `internal_error`. A `representation_busy` writer can retry after the current owner finishes. For `stale_revision`, recovery must re-read canonical state: re-read the clip, re-apply the edit, send it again with the new revision. Server commands preserve the server's shared stable registry, including authentication, protocol, mount, job, backup, and security codes. The registry is [`core/rcdvlog-server/src/error_codes.rs`](../core/rcdvlog-server/src/error_codes.rs); clients must branch on `error.code`, not prose. Remote failures also return the server `requestId` when available.

Proxy and thumbnail generation capture FFmpeg diagnostics and keep failures inside the JSON `error.message` with the stable `media_tool_failed` code. Proxy stderr is drained concurrently with its progress stream; thumbnail stderr is drained concurrently with the child process. Diagnostics redact absolute paths and are bounded to 4 KiB after redaction, so failures never prepend raw tool output to stderr.

Long ingest commands emit JSON progress objects to stderr every 25 assets and on completion:

```json
{"event":"ingest.progress","completed":25,"total":273,"path":"/Volumes/Card/clip.mp4"}
```

## Commands

### Prerequisites

```bash
rcdvlog doctor
rcdvlog --catalog /absolute/project/catalog.sqlite3 doctor --check
rcdvlog --catalog /absolute/project/catalog.sqlite3 doctor \
  --require-tool ffmpeg --require-models
rcdvlog --catalog /absolute/project/catalog.sqlite3 doctor --require-pipeline-smoke
```

`doctor` succeeds as a report even when the selected environment is not ready.
Its `data.executable` contains the canonical path of the running binary,
compile-time version/revision/dirty/profile/target/rustc metadata, capabilities,
and leaf command names. The `tools`, `models`, and `pipelineSmoke` sections
report observed readiness, while `catalogStatus` and the smoke receipt lookup
are read-only. `pipelineSmoke` is `unverified` until an explicit generated-media
run passes for the exact executable, FFmpeg/FFprobe, and recipe identities;
`doctor --require-pipeline-smoke` requires that pass. `--check` is strict: it
requires a compatible explicit catalog, all three supported media tools,
installed speech models, and a passed pipeline smoke receipt. Repeat
`--require-tool <name>`, `--require-models`,
`--require-capability <capability-or-exact-command>`, and
`--expect-revision <full-revision>` to ask for a narrower explicit contract.
Strict failures use JSON error codes such as `catalog_missing`,
`catalog_upgrade_required`, `media_prerequisite_failed`, `model_unavailable`,
`capability_unavailable`, and `revision_mismatch`; readiness errors include
the report in `error.details`. Model readiness reuses the existing presence
and configured-checksum policy. It does not load an inference engine or fetch
models; provisioning remains explicit.

### Speech model management

```bash
# Existing local/offline default: ASR + Silero VAD.
rcdvlog --catalog /absolute/project/catalog.sqlite3 models status
rcdvlog --catalog /absolute/project/catalog.sqlite3 models install

# Explicit native backend: only Silero VAD is local. Provider/model are required.
rcdvlog --catalog /absolute/project/catalog.sqlite3 models status \
  --backend native-openrouter --provider openrouter --model openai/whisper-1
rcdvlog --catalog /absolute/project/catalog.sqlite3 models install \
  --backend native-openrouter --provider openrouter --model openai/whisper-1
```

The default backend remains `local-whisper`, so old invocations retain the
historical two-row status and pair installation behavior.
`--backend native-openrouter` is an explicit model-readiness operation only: it
validates the registered provider-qualified audio model in shared Rust code,
reports only the Silero VAD row, and does not require or inspect the local
574 MB ASR file.
It performs no provider request, does not require an API key, and does not load
an inference engine. Unsupported, chat-model, or unqualified selections fail
before catalog or model-directory effects. Passing `--provider`/`--model` with
the local backend is rejected rather than ignored. The executable advertises
`speech.model-backends.v1` while preserving the leaf command identities
`models.status` and `models.install`.

`models status` is strictly read-only and offline. For each speech artifact it
reports a stable `name`/`role`, `present`, `ready`, `installed`, `integrity`
(`verified`, `present_unverified`, `invalid`, or `missing`), an actionable
`detail`, the expected `path`, and the catalog-managed `directory`. A missing
native VAD row names the explicit native install command in its `detail`; it
never directs native users to the generic local pair download. Status never
creates the model directory, migrates the catalog, downloads a file, or loads an
inference engine.

`models install` is the separate explicit provisioning operation. It downloads
only missing or invalid artifacts selected by the backend, emits
`models.install.progress` JSON events on stderr, and checks for Ctrl-C through
the existing bounded-callback path. Cancellation is checked before each download
and at transfer progress callbacks; it removes the partial file and returns a
success envelope with `cancelled: true`. Earlier completed downloads remain
installed. Transcription never calls this operation, so a batch with unavailable
models fails with `speech_models_not_ready` instead of starting a hidden
download.

### Project-local CLI binding

The binding records which absolute executable and existing catalog an agent
selected for a project root. It is stored only at
`<project-root>/.vlogcutter/cli-binding.json`:

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 workspace refresh \
  --root "/absolute/project root" \
  --executable /absolute/checkout/target/debug/rcdvlog \
  --selection development
rcdvlog --catalog /absolute/project/catalog.sqlite3 workspace check \
  --root "/absolute/project root" --check
```

Use `--selection installed` with an installed binary, or `development` with a
binary built from the intended checkout and pinned toolchain. Refresh requires
absolute paths, probes the exact selected executable, checks its versioned
doctor contract, and records its build identity and BLAKE3 digest. It can bind
an unready environment so setup can happen first; pass the same readiness
flags (`--check`, `--require-tool`, `--require-models`,
`--require-capability`, `--expect-revision`) when refresh must enforce them.
Check verifies the existing binding and returns an `invocation` string array
containing the selected executable, `--catalog`, and catalog path. Treat this
as structured argv; never concatenate it into a shell command. Without strict
readiness flags, `check` requires the bound compatible catalog and executable
contract but does not require media tools or speech models. Use `--check` before
scheduling work that needs the full tool and model set, or provide only the
specific `--require-*` options that operation needs.

Check rejects moved, missing, replaced, or incompatible executables without
falling back to PATH or a desktop installation. Repair is explicit: build or
select the intended executable, then run `workspace refresh` again. It does
not rewrite authored `AGENTS.md` or `PROJECT.md`, materialize any workspace
instructions, or change unknown files in `.vlogcutter/`.

Binding failures use `binding_missing`, `binding_invalid`, `binding_stale`,
`binding_catalog_mismatch`, `executable_unavailable`, or
`capability_unavailable`. Preserve an unrecognized binding file before manual
repair; refresh refuses to overwrite it.

### Screenshot-driven diagnosis

The desktop persists the last displayed catalog, project, and clip whenever selection changes. This is a diagnostic breadcrumb, not a live UI-control channel. Start screenshot investigations with:

```bash
rcdvlog desktop last-opened
rcdvlog --catalog /absolute/project/catalog.sqlite3 catalog inspect
rcdvlog --catalog /absolute/project/catalog.sqlite3 project diagnose --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips diagnose --asset <asset-id>
```

`catalog inspect`, `project diagnose`, `clips diagnose`, and `proxy status` open SQLite strictly read-only and never run migrations. Catalog inspection includes unresolved operation-conflict counts for operation-aware catalogs. Project diagnosis includes operation/conflict state, and clip diagnosis includes materialized revision plus immutable note history alongside the stored asset row, live ffprobe evidence, cache-version classification, computed render transform/final dimensions, warnings, and recommended explicit actions. It is the preferred evidence bundle when a screenshot disagrees with catalog labels.

The pattern is intentionally extensible: persist a stable ID for newly selectable UI entities, add a focused read-only `diagnose` projection in the Rust core/CLI, report stored state separately from observed external state, explain the derived behavior, and expose mutations as separate commands.

### Catalog inspection

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 catalog inspect
```

Reports required tables, pending diagnostic/schema columns, journal mode, project/asset counts, and unresolved operation conflicts without changing the file.

### Projects

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 project create --name "Expo 2025"
rcdvlog --catalog /absolute/project/catalog.sqlite3 project list
rcdvlog --catalog /absolute/project/catalog.sqlite3 project status --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 project diagnose --project <project-id>
```

### Ingest

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 ingest \
  --project <project-id> --folder "/absolute/source/Trip Footage"
```

Ingest references media in place and is idempotent within a project by sampled fingerprint.

Known content found somewhere new is not discarded. Ingest records the discovered
location, and repairs the asset when its recorded original had gone offline — so
re-running ingest against a card that remounted elsewhere reconnects it. The
report separates `imported`, `duplicates`, and `relinked`.

Ingest requires ffprobe to report a video stream with dimensions, codec, frame
rate, and a positive duration. Reachable files with no stream are skipped,
preserved in place, and recorded as per-file `ingest.no_video_stream`
diagnostics; streams missing required fields receive per-file
`ingest.invalid_video_stream` diagnostics. Both include byte reachability and
failure evidence, and neither kind creates an asset or counts as imported. For older catalogs,
`clips diagnose --asset <id>` reports byte reachability separately from usable
video and recommends keeping a no-video row out of review and chronology.
Migration leaves preexisting rows `unverified` because it cannot inspect
originals. `project status` and analysis readiness distinguish reachable source
bytes from usable, unverified, quarantined, and unavailable video counts. Only
validated usable clips enter analysis totals. Run
`clips validate-video --asset <id>` to persist a live source check. Offline
originals can be checked through an existing proxy. A prior usable validation
remains useful while its proxy is available.

### Camera identity repair

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 ingest --project <project-id> \
  --folder /absolute/media --camera "DJI Pocket 3"
rcdvlog --catalog /absolute/project/catalog.sqlite3 cameras repair-preview \
  --project <project-id> --camera "DJI Pocket 3" --asset <asset-id> --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 cameras repair --preview /tmp/camera-repair.json
```

`ingest --camera` is an explicit per-run identity choice for newly imported
clips. For an existing catalog, save the `repair-preview` JSON verbatim and
pass that file to `cameras repair`; stale previews are refused. Repair updates
only the selected assets and their filename counters. Partial card selections
receive a separate capture sequence, preserving the other assets and card
counter spaces. A NULL counter is filled only when the preserved filename
matches a known camera pattern.

### Camera sidecars

`camera-proxy discover --project <id> --card <root>` checks same-directory `.LRF`
companions through recorded same-asset original locations, including verified
alternates when the preferred original has been organized elsewhere.
`camera-proxy register --asset <id> --card <root> --sidecar <path>` explicitly
verifies and atomically registers the camera representation under the proxy
claim. `camera-proxy status --asset <id>` reports bytes, availability and saved
association/content/rate/time provenance. `camera-proxy frame-pack --asset <id>`
returns a representation-aware manifest with source-relative sample timestamps
and a fingerprint of the accepted bytes and mapping, including with offline
originals. Discover, status and frame-pack are read-only; they never migrate a
catalog or make a provider request. Generated-proxy readiness stays separate.
See [camera sidecar verification](camera-proxy-sidecars.md) for supported integer
rate reductions, rejection reasons and legacy behavior.

### Smart ingest camera mappings

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 camera-mappings list \
  --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 camera-mappings rename \
  --project <project-id> --mapping <mapping-id> --camera "Sony A7C II"
rcdvlog --catalog /absolute/project/catalog.sqlite3 camera-mappings clear \
  --project <project-id> --mapping <mapping-id>
```

`camera-mappings list` is strictly read-only and returns the stable mapping ID,
folder hint, filename prefix, representative EXIF make/model, resolved camera,
and creation/update timestamps. The evidence is project-scoped and excludes an
ephemeral mount path, which is why a matching reformatted card can reuse it.

`rename` changes only the camera identity one stored evidence key resolves to.
`clear` forgets that key without touching clips, capture sequences, originals,
archives, or cache files; it is idempotent so retrying an already-completed clear
is safe. Both mutations require the project ID as well as the stable mapping ID,
so a mapping from another project cannot be renamed accidentally.

### Camera identity merge

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 cameras merge-preview \
  --project <project-id> --from "Sony wrong" --into "Sony A7C II"
rcdvlog --catalog /absolute/project/catalog.sqlite3 cameras merge \
  --project <project-id> --from "Sony wrong" --into "Sony A7C II"
```

`merge-preview` is strictly read-only. It confirms both cameras exist and
reports the source clip, capture-sequence, and Smart ingest mapping counts plus
the destination's existing capture-sequence count. `merge` changes camera
identity only: every source card keeps its capture-sequence ID, membership, and
clock epochs while joining the destination lane. Smart ingest mappings follow
the destination, and later replay or ingest that still names the old camera is
resolved through the durable project-local alias. Neither command changes
original, archive, or cache files.

### Relink

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 relink \
  --project <project-id> --folder "/Volumes/Shuttle/Trip Footage"
```

Finds originals a project has lost track of, after the user moved footage between
a laptop, an external drive, and a NAS. Matching is by content, never by filename
or path — two cards legitimately both contain `C0001.MP4`.

Unlike `ingest`, relink **never creates an asset**. Media in the folder belonging
to no asset in this project is counted as `unrecognized` and left alone, so
pointing at the wrong folder is a readable no-op rather than an import to undo.
Emits `relink.progress` events to stderr on the same 25-asset cadence as ingest.

`stillMissing` reports how many originals remain unreachable after the search,
and offline-volume diagnostics that the search resolved are acknowledged
automatically.

### Clips

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips list --project <project-id> --limit 100 --offset 0
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips list --project <project-id> --query "arrival"
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips list --project <project-id> --include-removed
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips get --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips diagnose --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips refresh-orientation --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 clips update \
  --asset <asset-id> \
  --notes "Arrival establishing shot" \
  --add-label arrival \
  --add-label establishing \
  --remove-label unusable \
  --removed false \
  --rotation-degrees 180 \
  --expected-revision 7
```

`clips update` is a patch operation: omitted fields retain their current values. The merged patch is converted to typed note/label/removal/rotation operations and committed atomically; unchanged fields emit nothing. Repeat `--add-label` or `--remove-label` to change multiple free-form labels. Label matching for removal is case-insensitive; the core trims, deduplicates, and validates labels. `--removed true` hides a clip from the default labeling queue without deleting its catalog row or source media. `clips list` excludes removed clips unless `--include-removed` is passed. `--rotation-degrees` controls the review-only orientation and accepts `0`, `90`, `180`, or `270`; it never changes the source file.

`--expected-revision` is the optimistic check, and an agent writing to a project a human may also have open should always send it. Pass the `revision` from the `clips get` (or `clips list`) the edit was decided from; the write is refused with `stale_revision` if the clip changed in between. This matters because the merged patch is a *whole* judgment: a patch built from a stale read carries the old label list and the old note, so applying it does not merely miss the other writer's change, it reverses it. The recovery is idempotent and cheap — read the clip again, re-apply the same edit to what it now says, and send it with the new revision. Omitting the flag keeps the previous last-write-wins behaviour for scripted writes with no basis to be stale against.

`clips refresh-orientation` is the explicit mutating counterpart to diagnosis. It probes the immutable original, marks orientation as known even when the answer is 0°, preserves a user override, and updates only catalog metadata.

`clips orientation-queue` reads a bounded review page. `clips orientation-evidence`
generates raw-metadata/effective comparison stills from an available proxy or original.
Use `clips orientation-confirm`, `orientation-unresolved`, or `orientation-confirm-batch`
with expected revisions to record explicit human decisions; a numeric override alone
does not establish visual confirmation. The CLI advertises `orientation.review.v1`.
See [orientation review](orientation-review.md) for batch arguments and preservation rules.

### Camera clock events

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 clock apply \
  --project <project-id> \
  --camera "DJI Pocket 3" \
  --start-filename DJI_20250810104709_0002_D.MP4 \
  --offset-minutes -60 \
  --label "Camera timezone changed"
```

The fixed offset applies from the filename boundary onward for that camera. It does not alter source metadata.

### Revision-safe chronology changes

Chronology facts can be changed through the typed core operations. Requests are
JSON on stdin (`--input -`) or in a request file. `chronology preview` is
read-only and returns a reusable `data` value for operations with core preview
support; send that exact value back as `preview` to `chronology apply`. Those
preview payloads include the core facts token, solver impact, and exact affected
identities. If the catalog changed, apply returns `chronology.stale_preview`
and writes nothing; preview again and review the new impact. Exact
split/reassignment selections remain pinned to their returned asset IDs, so
clips imported later are never added implicitly.

```bash
printf '%s\n' '{"kind":"split","sourceSequenceId":"<sequence>","atCounter":120,"label":"Second card","assetIds":["<clip-a>","<clip-b>"]}' |
  rcdvlog --catalog /absolute/project/catalog.sqlite3 chronology preview --project <project> --input -
rcdvlog --catalog /absolute/project/catalog.sqlite3 chronology preview --project <project> --input epoch-request.json
rcdvlog --catalog /absolute/project/catalog.sqlite3 chronology apply --project <project> --input saved-apply-request.json
```

Preview kinds are `split`, `reassign`, `merge`, `epochSet`, `epochClear`,
`sequenceDeclare`, `sameCard`, `sameCardRestore`, `exceptionSet`,
`exceptionClear`, `phaseSet`, `phaseRemove`, `phaseSplit`, `constraintSet`, and
`constraintClear`. Apply accepts each kind with its exact preview under
`preview`. Clock epoch requests use an `epoch` object matching the chronology
JSON fields. Phases use a `phase` object. A phase-split preview commits both
resulting phase operations in one transaction. No command edits raw timestamps.

Each CLI process commits an apply atomically. The direct-mutation preview API
recognizes retry of the same saved candidate after a lost response and returns
the applied mutation without appending another operation. Specialized sequence
and epoch previews reject reuse after state changes; after a lost response,
read the chronology facts before deciding whether to preview again. For a split
or reassignment, the selected IDs are explicit and cannot grow during retry.

### Proxies

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 proxy generate --asset <asset-id> --policy auto
rcdvlog --catalog /absolute/project/catalog.sqlite3 proxy generate --asset <asset-id> --policy software
rcdvlog --catalog /absolute/project/catalog.sqlite3 proxy missing --project <project-id> --policy auto
rcdvlog --catalog /absolute/project/catalog.sqlite3 proxy status --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 proxy status --project <project-id> --limit 100
rcdvlog proxy probe --policy auto
rcdvlog --catalog /absolute/project/catalog.sqlite3 proxy provenance --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 proxy history
```

`--policy auto` is the safe default: on Linux it uses only a capability after a
real bounded VAAPI decode/scale/encode probe, then retains software fallback.
`--policy software` never opens a DRM device and is deterministic for tests and
operators. `proxy probe` is a host diagnostic and deliberately does not require
an existing catalog. Its JSON separates device presence, device accessibility,
FFmpeg encoder advertisement, actual initialization testing, and usability, and
includes actionable status/detail for unavailable hardware.

`proxy status` is read-only and distinguishes verified raw-orientation v3
proxies, obsolete metadata-tagged raw v2 proxies, legacy metadata-baked v1
proxies, and missing files. Listed v3 items are live-probed and are ready only
when their display rotation is 0°. It also includes each item's latest recipe
and actual encoder provenance plus encoder-aware rate histories with an explicit
scope note. `proxy provenance` returns durable attempt/recipe/timing/failure/
fallback/validation evidence newest first. `proxy history` returns throughput
observations scoped by actual successful encoder and worker count; those rates
describe only this host/catalog's observed corpus mix and are not universal
benchmarks. `proxy missing` may be long-running and regenerates obsolete
representations when originals are available. Completed proxies remain committed
if the command is interrupted.

Proxy and thumbnail writes coordinate across CLI, desktop, and sync through OS file claims under the managed `cache/.representation-locks` directory, keyed by cache root, asset, and kind. Lock files persist after release and must not be deleted: removing one could let two processes own different inodes for the same representation. A contended single-item generate command exits nonzero with `error.code: "representation_busy"`; batch commands count contended work as skipped, without a failure or throughput sample. Proxy downloads acquire the claim before requesting content, stream into unique staging files, and publish through the same validation/registration path as generated proxies.

### Host resource calibration

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 calibrate capabilities
rcdvlog --catalog /absolute/project/catalog.sqlite3 calibrate status
rcdvlog --catalog /absolute/project/catalog.sqlite3 calibrate run --recipe proxy \
  --policy software --worker-counts 1 --trials 1 --generated-duration-seconds 1
rcdvlog --catalog /absolute/project/catalog.sqlite3 calibrate run --recipe speech \
  --vad-threads 1,2,4 --decode-threads 4 --trials 1
rcdvlog --catalog /absolute/project/catalog.sqlite3 calibrate run --recipe proxy \
  --media /absolute/authorized/representative.mp4 --trials 2
rcdvlog --catalog /absolute/project/catalog.sqlite3 calibrate status --recipe pipeline-smoke
rcdvlog --catalog /absolute/project/catalog.sqlite3 calibrate run --recipe pipeline-smoke
```

`calibrate capabilities` and `calibrate status` are read-only and never benchmark. `calibrate run` is explicit opt-in and bounded: generated synthetic media is the default, and `--media` is the only way to select a representative file. Representative media is rejected before full-file hashing/PCM expansion when it exceeds 30 seconds or the documented byte bound, and candidate/total-trial caps bound the matrix. A completed exact-key result is reused without rerunning the benchmark; `--force` requests an intentional recalibration. It never downloads missing speech models; a speech run without ready models reports `not_ready` and adopts nothing. Results are keyed by host, build, media-tool executable/version, device/model, recipe, representation kind, and candidate configuration. Only a complete matching result can seed a range-bearing ETA. Interrupted, failed, not-ready, stale, and explicit-override states are reported as uncertainty rather than a completion promise. Speech candidates are adopted only after their detected regions exactly match the validated baseline; the tool does not assume one VAD thread is universally optimal. VAD calibration fixes decode threads at the default because it does not measure decode quality; decode tuning is available only as an explicit, uncalibrated override. Proxy candidates retain the existing output validation and render/audio contract checks. Synthetic 320x180 proxy calibration reports `generatedContractOnly` compatibility and a deliberately wider ETA range; it is not presented as representative 4K/HEVC coverage.

`calibrate status --recipe pipeline-smoke` is a read-only readiness check for the exact running CLI executable, FFmpeg, FFprobe, and smoke recipe. A missing or identity-stale receipt reports `unverified`; a failed receipt reports its measured failures and remediation. The same status is included in `doctor` and the read-only `prepare preflight` manifest; `doctor --require-pipeline-smoke` refuses to report the environment ready until it passes. `calibrate run --recipe pipeline-smoke` explicitly generates short mixed-frame-rate, mixed-dimension, tone, no-audio, silent, and rotated fixtures in managed scratch, invokes the production preview renderer, decodes the rendered video and PCM, and stores a machine-readable receipt in the selected catalog. Exact identities reuse their receipt; `--force` reruns it. The check covers only this host's generated preview path and makes no native editor or Resolve compatibility claim.

Explicit runtime overrides are available without calibration:

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe generate --asset <asset-id> \
  --vad-threads 2 --decode-threads 4
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe batch --project <project-id> \
  --asset <asset-a> --asset <asset-b> --vad-threads 2 --decode-threads 4
rcdvlog --catalog /absolute/project/catalog.sqlite3 proxy missing --project <project-id> \
  --workers 2
```

Override values are bounded, provenance-bearing, and never relabelled as calibrated. A settings-qualified history key is used when VAD/decode/worker settings differ from defaults. One shared local-heavy slot serializes proxy/transcode, full-hash, transcription, and calibration entrypoints; safe lightweight, metadata, review, import, and cloud/network work may overlap. The CLI/server use an advisory same-host/same-user lock, which is not a distributed cluster lock.

The automated checks establish the deterministic reuse/invalidation, region-comparison, override, and scheduling rules plus a bounded real generated-media FFmpeg proxy benchmark. Real GPU/VAD/target-device throughput acceptance requires ready models and that specific hardware; absence is reported rather than fabricated.

### Thumbnails

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 thumbnails generate --asset <asset-id> --quality 1
rcdvlog --catalog /absolute/project/catalog.sqlite3 thumbnails missing --project <project-id> --quality 1
```

Quality 1 is a 480-pixel-wide JPEG sampled at 20% of the clip. It is stored under the project folder at `cache/thumbnails/<asset-id>/q1.v2.jpg`. The original is preferred; an available proxy is used when the original is offline. Qualities 2–10 are reserved and currently rejected.

Analysis-selected Q10 representative thumbnails also take the shared thumbnail claim. Their output recipe and `q10.v1.jpg` name remain unchanged.

### Local transcription and cloud clip analysis

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe generate --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe missing --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe batch --project <project-id> --asset <asset-a> --asset <asset-b>
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe batch --project <project-id> --asset <asset-a> --force
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe status --project <project-id>

```

Explicit native cloud transcription is a separate, provider-qualified backend. Local
`transcribe generate`/`missing`/`batch` remains the offline default. A native policy
selects the exact assets, provider, verified audio-transcription model, language,
provider data policy and whole-policy budget; `audio` is the only supported payload.

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 models install \
  --backend native-openrouter --provider openrouter --model openai/whisper-1

rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe native preflight \
  --project <project-id> --asset <asset-a> --asset <asset-b> \
  --backend native-openrouter --provider openrouter --primary-model openai/whisper-1 \
  --language en --zdr true --data-collection deny --budget-usd 1.00
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe native authorize \
  --project <project-id> --asset <asset-a> --asset <asset-b> \
  --backend native-openrouter --provider openrouter --primary-model openai/whisper-1 \
  --language en --zdr true --data-collection deny --budget-usd 1.00
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe native pilot \
  --project <project-id> --asset <asset-a> --asset <asset-b> \
  --backend native-openrouter --provider openrouter --primary-model openai/whisper-1 \
  --language en --zdr true --data-collection deny --budget-usd 1.00
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe native run \
  --policy-id <policy-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 transcribe native status \
  --project <project-id> --policy-id <policy-id>
```

The core opens a real accounted `native_speech_transcription` run only when a
missing bounded window actually needs a provider send. Local Silero VAD runs on
the approved derived proxy first; NoAudio/NoSpeech and fully cached resumes make
no provider request and need no credentials. Every paid send uses the same
whole-policy budget authority and reservation/receipt/recovery lifecycle as
other analysis work. A paid pilot window is reused through its exact canonical
request/payload identity before another upload. Dispatched/uncertain/receipt-only
work stops for inspection and reconciliation rather than blind replay; a rerun
resumes only missing typed windows. `status` reads the canonical transcript plus
native provenance, including per-window quality, actual provider/model, latency,
confirmed cost and separately retained uncertain usage. The native batch path
does not load the local Whisper ASR model, does not fall back to local ASR, and
does not silently substitute a chat model for the audio-transcription endpoint.

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze generate --asset <asset-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze missing --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze missing --project <project-id> --force
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze missing --project <project-id> --primary-model google/gemini-2.5-flash-lite --allow-model google/gemini-2.5-flash-lite --no-fallback
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze suggest-chronology --project <project-id> --asset <asset-a> --asset <asset-b>
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze status --project <project-id>

rcdvlog --catalog /absolute/project/catalog.sqlite3 labels consolidate --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 labels vocabulary --project <project-id>
```

`transcribe batch` is the incremental preparation surface. It validates the
entire explicit selection before work starts: an empty selection, duplicate
asset IDs, unknown IDs, and IDs owned by another project fail with
`empty_asset_selection`, `duplicate_asset_id`, `unknown_asset_id`, or
`asset_project_mismatch` respectively in the core API. The CLI requires at least
one `--asset`, reporting `invalid_arguments` for an omitted selection. The
selection is capped at 10,000 assets (`selection_too_large`); a missing project
reports `project_not_found`.
Only the requested project-owned assets are touched.

Current fingerprinted transcripts are reused by default; `--force` explicitly
regenerates them. Failed attempts stay retryable and are never reported as reused.
Transcript replacement never changes human-authored notes,
labels, titles, descriptions, removals, rotation, or chronology. Removed assets,
videos not durably validated as usable, and assets without a reachable proxy are
reported per asset as `not_ready` and skip engine work. Audio presence is probed
before PCM extraction: a valid video with no audio stream is `no_audio`, requires
no speech model or engine initialization, and is reused on later runs. A probe or
extraction failure is `failed`, not silence. Missing or invalid speech models are
a run-level readiness failure before any engine initialization for assets that
actually contain audio. For ready assets, one engine lifecycle serves the
compatible batch. The report has per-asset states (`transcribed`, `no_audio`,
`reused`, `not_ready`, `failed`, `cancelled`), counts, and
`cancelled`/`cancelledAssets`; progress is emitted as bounded
`transcribe.batch.progress` JSON events on stderr, including a one-worker ETA
based only on attempted transcription work. Ctrl-C stops after the current asset,
preserves completed transcripts, and reports the remaining assets as cancelled.

Transcript results carry `status` plus `outcomeReason`. `no_speech` means the VAD
gate ran and found no detected speech; `no_audio` means the media had no audio
stream; `no_accepted_speech` means speech was detected but the provider returned
no accepted text; `rejected` means the quality gate discarded output; and
`failed` is retryable. The result also retains the exact source fingerprint,
detected `speechSeconds`, provider `windowCount`/`providerSegmentCount`, and
quality evidence (`meanTokenProbability`, `repetitionRatio`). Per-segment and
aggregate `meanTokenProbability` are JSON numbers when local token evidence was
actually measured and JSON `null` when it was absent; `null` is never confidence
zero. Non-empty candidate output whose probability coverage is incomplete is
rejected with `missing_probability_evidence`, and malformed supplied values with
`invalid_probability_evidence`. `transcribe status`
reports `noAudio` and `noAcceptedSpeech` in addition to speech, no-speech,
rejected, failed, and missing coverage. `legacyNoSpeechUnverified` is counted as
a subset of no-speech coverage. Rows migrated from older catalogs keep
`no_speech` with reason `legacy_no_speech_unverified`; preparation marks them
review-required rather than asserting verified silence.

Transcription is offline and reads proxies only. Vision analysis reads proxy-derived frame packs and any accepted speech transcript, then calls OpenRouter using `OPENROUTER_API_KEY` or `~/.openrouter.key`. A normal `analyze missing` run is idempotent; `--force` replaces current derived observations and reports how many clips already had non-empty title or description fields before overwrite. Notes remain human-only. OCR is a reviewable suggestion and is not indexed as fact.

`analyze status` and `labels vocabulary` are strictly read-only. Analysis progress is emitted as JSON on stderr.

### Bounded within-clip moment discovery

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 moments plan \
  --project <project-id> --asset <asset-id> \
  --provider openrouter --model qwen/qwen3.7-flash --payload frames \
  --budget-usd 0.25
rcdvlog --catalog /absolute/project/catalog.sqlite3 moments discover \
  --project <project-id> --all-usable --provider openrouter \
  --model qwen/qwen3.7-flash --payload frames-and-transcript --budget-usd 2.00
rcdvlog --catalog /absolute/project/catalog.sqlite3 moments discover \
  --project <project-id> --asset <asset-id> --provider openrouter \
  --model qwen/qwen3.7-flash --payload frames --budget-usd 0.25 \
  --force --retry-uncertain
rcdvlog --catalog /absolute/project/catalog.sqlite3 moments authorize \
  --project <project-id> --asset <asset-id> --provider openrouter \
  --model qwen/qwen3.7-flash --payload frames --budget-usd 0.25
rcdvlog --catalog /absolute/project/catalog.sqlite3 moments pilot \
  --project <project-id> --asset <asset-id> --provider openrouter \
  --model qwen/qwen3.7-flash --payload frames --budget-usd 0.25
rcdvlog --catalog /absolute/project/catalog.sqlite3 moments run \
  --policy-id <policy-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 moments list \
  --project <project-id> --offset 0 --limit 20
rcdvlog --catalog /absolute/project/catalog.sqlite3 moments get \
  --asset <asset-id> --offset 0 --limit 8
```

`moments plan` validates the selected project, assets, model allowlist,
permitted fallbacks, payload and budget without credentials, provider calls, or
catalog writes. Its `runPolicy` member is the exact content-addressed policy used
by `authorize`, `pilot` and `run`. `discover` requires the
same explicit project and either one or more repeated `--asset` values or
`--all-usable`; the latter selects non-removed assets with usable recorded video
validation in that project. There is no implicit desktop/global asset scope.
The provider is currently OpenRouter and the model must be in the existing
approved model registry. `--budget-usd` is required and must be finite and
positive. Requested provider and the provider actually reported by the selected
endpoint are recorded separately; missing upstream identity stays missing.
The artifact also records whether its selected payload included frames alone
or frames with accepted timed transcript evidence.
Moment prompt/schema v2 bind every proposed core, payoff, setup and context to
that request's exact inspected source window, in integer source milliseconds.
The wire schema and local validator use the same lower and upper bounds. A
refinement seed carries event/range/reason hints; earlier frame or transcript
citations are not evidence in the new request. Frames-only prompts prohibit
speech claims, and only supplied evidence keys may be cited. Rejected replies
remain rejected with their original provider body retained in local SQLite for
inspection. Changed versions invalidate previous request/policy/cache identities.
Sparse inspection still requires playback before accepting final cut boundaries.

The defaults are 96 coarse frames, 384 refinement frames, at most 8
candidates per asset, and an edit profile of 3–8 seconds for selects and 20–30
seconds for justified holds. These frame limits are bounded request budgets,
not continuous decoding guarantees. Sampling reports its measured sparse gaps;
the final timestamp is backed off by at least one proxy frame period so the
extractor requests a decodable frame near the tail. This does not certify the
unobserved interval or every possible cut.

Use `--payload frames-and-transcript` to include only accepted, timed speech
derived from the current proxy. `--force` explicitly replaces completed derived
evidence. A pending or interrupted request with unknown usage blocks automatic
duplicate calls; `--retry-uncertain` is the explicit recovery authorization and
should be used only after reviewing the prior report. A completed run can be
`ready`, `no_candidates`, `needs_review`, or `unavailable`. `ready` means the
bounded evidence passed the current checks; sparse frame sampling never
certifies every instant or a final edit cut. Candidates are suggestions, and
human review of timing, interpretation, and edit handles remains a separate
acceptance step. `moments list` and `moments get` are read-only paginated views;
their offsets page candidates, coverage, request receipts, and review questions
independently, with totals and `nextOffset` for continuation. Discovery progress
is emitted as JSON lines on stderr.

### Preparation evidence for editing

Preparation adds a bounded, local-first view over proxy validation, timed
transcript provenance, sparse moment suggestions, and orientation/review
questions. Every command requires an existing explicit project catalog via
`--catalog` or `RCDVLOG_CATALOG`; it never consults the desktop breadcrumb or
creates a missing catalog.

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 prepare preflight \
  --project <project-id> --asset <asset-id> --offset 0 --limit 10
rcdvlog --catalog /absolute/project/catalog.sqlite3 prepare preflight \
  --project <project-id> --all --offset 10 --limit 10
rcdvlog --catalog /absolute/project/catalog.sqlite3 prepare evidence \
  --project <project-id> --asset <asset-id> --kind transcript --offset 0 --limit 10
rcdvlog --catalog /absolute/project/catalog.sqlite3 prepare evidence \
  --project <project-id> --asset <asset-id> --kind moments --offset 0 --limit 10
rcdvlog --catalog /absolute/project/catalog.sqlite3 prepare run \
  --project <project-id> --asset <asset-id> --transcribe
rcdvlog --catalog /absolute/project/catalog.sqlite3 prepare run \
  --project <project-id> --asset <asset-id> --moments \
  --provider openrouter --model qwen/qwen3.7-flash \
  --payload frames-and-transcript --budget-usd 0.25
```

`prepare preflight` and `prepare evidence` open SQLite read-only: they do not
migrate the catalog, validate media by decoding, write cache state, download
models, or contact a provider. Pages have `limit` 1–50 and `offset` at most
1,000,000. Manifest `pageOutcome` describes only the returned page, while its
scope fingerprint identifies the selector, not a frozen project snapshot.
`--all` includes removed/excluded rows so the inventory remains truthful.
Moment evidence pages reuse the existing bounded moments projection.

`prepare run` is the only mutating preparation command. For paid moment
discovery, pass `--moments --policy-id <policy-id>` with an explicit `--asset`
scope that exactly matches the authorized policy; provider/model/payload/budget
overrides are refused. With no stage flags it
validates/reuses existing local proxies and current receipts; `--proxies`,
`--transcribe`, and `--revalidate` explicitly add local work. `--moments` is the
only paid-work authorization and requires the provider, model, payload, and
positive USD budget together. It reuses the approved model validation and one
budget over the eligible explicit scope. Models must already be installed for
transcription. Missing usage receipts are not retried implicitly; use the
existing explicit `moments` recovery commands after reviewing unknown charges.

The manifest distinguishes the actual proxy recipe from the required current
recipe and binds validation to full content identity. Existing transcript
bindings cannot be carried across a full proxy replacement. Source bytes,
human notes/labels/title/removal, rotation, and chronology remain untouched by
preparation. A `ready` result means the available technical evidence passed
its checks; sparse moment samples are suggestions and do not certify an
arbitrary source interval or edit cut. Synthetic fixture tests verify these
workflow contracts, not transcription quality or subjective moment selection.

### Analysis policy, authorization and pilot

Every paid batch should first be described and authorized as one exact
`RunPolicy`. The policy includes the project-owned asset IDs and source/
representation fingerprints, explicit model allowlist and permitted fallbacks,
payload, expected cost, requested budget, prompt/schema/recipe/tool versions and
readiness. Credentials are reported only as present or absent.

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze preflight \
  --project <project-id> --asset <asset-a> --asset <asset-b> \
  --provider openrouter --primary-model qwen/qwen3.7-flash \
  --budget-usd 0.50 --payload frames-and-transcript --project-vocabulary
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze authorize \
  --project <project-id> --asset <asset-a> --asset <asset-b> \
  --provider openrouter --primary-model qwen/qwen3.7-flash \
  --budget-usd 0.50 --payload frames-and-transcript --project-vocabulary
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze pilot \
  --project <project-id> --asset <asset-a> --asset <asset-b> \
  --provider openrouter --primary-model qwen/qwen3.7-flash \
  --budget-usd 0.50 --payload frames-and-transcript
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze run \
  --policy-id <policy-id>
```

Authorization records approval of the exact scope, payload, model allowlist,
fallbacks and budget. It is separate from the pilot, which is one bounded
capability/schema/source-time observation and is never a guarantee of future
provider availability or semantic correctness. A sample/single-clip pilot does
not authorize any other clip. Budget approval does not authorize a different
model or payload. Any source, model, payload, budget or schema/prompt/tool
version change produces a different policy ID and invalidates existing approval
and pilot records. The policy also records whether the exact project vocabulary
is attached: omit `--project-vocabulary` to keep a request frames/transcript-only.
A frames payload never reads or sends a transcript. Pilot spend counts against
the same run ceiling. Retrying an interrupted or unknown-usage pilot retains the
prior known cost and can never re-enable bulk work until the uncertainty is
resolved. Missing credentials fail before provider construction. Existing
explicit `analyze missing`/desktop batch invocations materialize the same
policy, authorization and pilot automatically before bulk work.

See [`docs/analysis-preflight.md`](analysis-preflight.md) for the data model and
limits. `moments plan/authorize/pilot/run` expose the same lifecycle for
editorial moment discovery; `moments plan` includes the full `runPolicy`.

`analyze suggest-chronology` makes one bounded, read-only visual evidence request
for 2–8 selected clips, using at most two selected proxy frames per clip. It
returns coverage diagnostics and checkable conflicts against the current solved
chronology. The result is stored separately from chronology facts and keyed by
the facts revision, proxy frames, and model policy; repeating a completed
request returns the saved result without another provider call. Each actual
transmission is admitted by the canonical accounting owner. An interrupted or
uncertain request must be inspected and reconciled rather than resent blindly.

### Analysis accounting inspection and recovery

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze accounting inspect \
  --project <project-id> --operation <operation> --request <request-key>
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze accounting reconcile \
  --project <project-id> --request <request-key> \
  --receipt-file /absolute/trusted-receipt.json
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze accounting reconcile \
  --project <project-id> --request <request-key> --local-no-upload \
  --failure-code local_extraction_failed
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze accounting reconcile \
  --project <project-id> --request <request-key> --unknown-outcome
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze accounting retry-authorize \
  --project <project-id> --request <request-key>
rcdvlog --catalog /absolute/project/catalog.sqlite3 analyze accounting targeted-retry-dispatch \
  --project <project-id> --request <request-key> \
  --expected-next-attempt-epoch 1 \
  --payload-file /absolute/original-vlm-request.json
```

`inspect` is read-only and supports project, policy, operation, run and request
selection plus bounded pagination. When a request or run is selected, only its
matching budget scope or scopes are returned rather than every authority in the
project. It reports canonical confirmed, active reservation and
unresolved-uncertainty totals separately, together with
append-only events, provider receipts, prior attempt epochs and provider-derived
actual model/generation provenance. It never emits prompt, frame or response-body
content. Reconciliation is offline and takes exactly one trusted evidence mode:
a typed receipt file, a marker-absent local no-upload conclusion, or an explicit
unknown outcome. Repeated identical reconciliation is idempotent. A reconciled
pilot is projected conservatively from its existing validation/history and does
not become a capability pass.

Targeted retry has two mandatory steps: durable `retry-authorize` for that
particular request and epoch, then a separate `targeted-retry-dispatch` naming
the expected epoch and the exact original camel-case `VlmRequest` JSON file. The
stored payload digest cannot reconstruct missing bytes. The core rechecks
project, policy/source/vocabulary, operation, requested route/model, exact
payload, current provider cost-bound contract and attempt bounds before
credentials or transport. It reuses the prior immutable attempt base, retains all
earlier costs/uncertainty, allows at most four attempts including the first and
rejects stale/concurrent epoch consumers. Only one original-model transmission
is made; no whole-chain retry or generic force path exists. The returned body is
in that command's JSON only and is not persisted or applied automatically.

`analyze missing` accepts `--primary-model`, repeatable `--allow-model`, and
`--no-fallback`. The core validates this policy against its local model registry
before resolving credentials or constructing a network provider. The desktop
run request uses the same fields. Each concurrent worker/process atomically reserves the complete conservative
provider-owned request bound in project SQLite before transport. A request is
admitted only when that full bound fits the remaining whole-policy exposure; the
in-flight cap is four. This prevents independent remaining-spend checks from
oversubscribing the same budget. Provider-returned cost above the admitted bound
is retained as confirmed evidence and durably stops further dispatch. Budgets
must be finite and greater than zero; zero and negative values are rejected
before credentials or provider setup. The report records returned usage for
successful and domain-rejected requests, including fallback attempts. A request
with a failed provider attempt retains honest uncertainty; known usage is kept
and no cost is inferred for failed attempts.
Per-clip analysis records persist `attemptedModels` and `usageComplete` with the
model, attempt count, and returned usage. Existing single-attempt rows backfill
their serving model; prior retry histories are not reconstructable, so those
rows keep an empty attempted-model list and mark usage incomplete.

`transcribe missing` and `analyze missing` reports include a `cancelled` flag,
and the analysis report also includes `budgetExhausted`. `cancelled` is always
`false` from the CLI, which does not expose cancellation. The CLI honors
`analyze missing --budget-usd`; `budgetExhausted` is true when returned usage
reaches the ceiling. The ceiling is checked after each completed request, so
already in-flight requests can finish first. A budget stop additionally appends
a failure entry naming the ceiling, while a cancellation does not — stopping on
request is not a failure. `labels consolidate` makes one text-only request and
remaps only labels whose local authorship record says analysis created them; it
never changes human-only labels.

Rough-cut selection and source ranges remain separate from the labeling phase.
The supported immutable edit-plan commands below create, compare, validate,
activate, preview and inspect plans/source windows. Export/history CLI and the
project-local agent workspace remain follow-up work under #48. Editing a
projected `edit-plan.json` by hand does not mutate a catalog.

## Companion-server commands

Every remote command requires an explicit endpoint, either `--endpoint` or `RCDVLOG_SERVER_ENDPOINT`:

```bash
rcdvlog server --endpoint https://<server-host>:8443 status
```

Credential precedence is:

1. `RCDVLOG_SERVER_TOKEN`.
2. `--credential-file` or `RCDVLOG_SERVER_CREDENTIAL_FILE`; the file must be owner-only.
3. Interactive macOS Keychain lookup for the returned stable server node ID.

Prefer an owner-only credential file for unattended automation. Never put the token in argv, a URL, output, a fixture, or a repository file. Every success includes request endpoint, non-secret credential-source label, protocol/version identity, and result.

All list operations are bounded and cursor-based where the API supports pagination:

```bash
rcdvlog server --endpoint https://<server-host>:8443 projects --limit 50
rcdvlog server --endpoint https://<server-host>:8443 library list --limit 50 --filename-prefix DJI --camera "Pocket 3"
rcdvlog server --endpoint https://<server-host>:8443 library diagnose --media-object <media-object-id>
rcdvlog server --endpoint https://<server-host>:8443 storage list
rcdvlog server --endpoint https://<server-host>:8443 storage diagnose --root <root-id>
rcdvlog server --endpoint https://<server-host>:8443 scans list --limit 50
rcdvlog server --endpoint https://<server-host>:8443 scans diagnose --scan <scan-id>
rcdvlog server --endpoint https://<server-host>:8443 jobs list --limit 50 --state failed
rcdvlog server --endpoint https://<server-host>:8443 jobs diagnose --job <job-id>
rcdvlog server --endpoint https://<server-host>:8443 sync status
rcdvlog server --endpoint https://<server-host>:8443 sync diagnose --project <project-id>
rcdvlog server --endpoint https://<server-host>:8443 representations status --representation <representation-id>
rcdvlog server --endpoint https://<server-host>:8443 credentials list
rcdvlog server --endpoint https://<server-host>:8443 backups list
rcdvlog server --endpoint https://<server-host>:8443 backups diagnose --backup <backup-id>
```

Read commands may inspect already-known filesystem/network/tool state but never migrate, scan broadly, hash a full file, enqueue, repair, change cursors, or create cache output. Their JSON separates stored state, live observation, derived decisions, version identity, warnings, and recommended explicit mutations.

Mutations are deliberately named:

```bash
rcdvlog server --endpoint https://<server-host>:8443 scans request \
  --root <root-id> --scope <relative/subtree> --idempotency-key <unique-key>
rcdvlog server --endpoint https://<server-host>:8443 jobs retry --job <job-id>
rcdvlog server --endpoint https://<server-host>:8443 jobs cancel --job <job-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 server \
  --endpoint https://<server-host>:8443 sync now --project <project-id>
rcdvlog server --endpoint https://<server-host>:8443 representations request \
  --media-object <media-object-id> --kind proxy
rcdvlog server --endpoint https://<server-host>:8443 credentials pairing-create \
  --output-file /absolute/new/pairing-code.txt
rcdvlog server --endpoint https://<server-host>:8443 credentials revoke --credential <credential-id>
rcdvlog server --endpoint https://<server-host>:8443 backups create --idempotency-key <unique-key>
```

Pairing-code creation removes the plaintext code from JSON and writes it once to a new owner-only file. `sync now` signals the running desktop when it owns the project sync manager; otherwise it performs one exclusive bounded reconciliation and reports which path ran.

### Waiting and callbacks

For long jobs, use `server ... jobs wait --job <id> --wait-seconds 1800` to
receive one terminal JSON result without agent-driven polling. Durable
`jobs callbacks register/list/deliver` commands support external consumers via
opaque targets and a `job.completion.v1` stdin envelope. See
[agent waits and callbacks](server/job-callbacks.md) for complete examples,
stable timeout/error codes, leases and at-least-once delivery semantics.

## Server-local administration

`rcdvlog-server admin` is the offline, server-host-only recovery surface. It uses environment-selected data/backup roots, returns one JSON result, and refuses exclusive work against a running server.

```bash
rcdvlog-server admin bootstrap create --output-file /absolute/new/bootstrap-code.txt
rcdvlog-server admin backup inspect --backup /absolute/backup/restore-point
rcdvlog-server admin restore inspect --restore-point /absolute/backup/restore-point
rcdvlog-server admin restore apply \
  --restore-point /absolute/backup/restore-point \
  --new-data-root /absolute/new-server-data \
  --confirm-new-data-root
rcdvlog-server admin diagnostics bundle --output-directory /absolute/new/diagnostic-directory
```

Restore always targets a new directory and requires the server to be stopped. Sequence-divergence acceptance is intentionally omitted from routine examples; use it only after following the investigation in [backup-and-recovery.md](server/backup-and-recovery.md).

## Concurrency

Desktop and server SQLite files use WAL mode only for same-host concurrency. Read commands can run while another local connection holds an uncommitted write. Cross-node concurrency uses immutable typed operations, per-origin cursors, deterministic materialization, and explicit conflicts—not shared SQLite files. Avoid competing full-patch desktop/CLI edits to the same asset fields; use the operation-specific server/web mutations where available.

Calibration status and selection compare lightweight model file identity;
full model content digests are evidence collected only by explicit calibration.
Queued proxy, transcription and preparation batches expose
`waiting_for_resource` progress and honor cancellation before opening an engine
or probing an encoder. Process-local waiters are served LIFO; independent
processes sharing the same lock path have mutual exclusion without a global
ordering guarantee. Set `RCDVLOG_RESOURCE_LOCK_ROOT` to a shared host-visible
location when crossing service/container filesystem namespaces. VAD calibration
keeps decode threads at four because it does not measure Whisper decoding;
decode thread overrides remain explicit, uncalibrated settings.

Generated calibration timing is evidence about its fixture, not a byte-rate
prediction for unrelated footage. Runtime ETA starts uncertain until compatible
sample timing or configuration-matching observations of actual work exist.
Calibration can still recommend bounded resource settings without inventing a
corpus completion time.


## Immutable edit plans

Always supply an existing explicit `--catalog`. Read-only commands open without
migrations and return the ordinary JSON envelope. Commands:

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan list --project <project-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan get --revision <revision-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan create --project <project-id> --input /absolute/plan.json --expected-base none --expected-active none
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan diff --base <revision-a> --revision <revision-b>
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan validate --revision <revision-id>
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan activate --revision <revision-id> --expected-active none
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan preview --revision <revision-id> --max-height 360
```

The complete ordered input is an object (maximum 4 MiB and 10,000 cuts):

```json
{
  "operations": [
    {"operationId": "opening", "position": 0, "assetId": "asset-id", "sourceInSeconds": 0.25, "sourceOutSeconds": 2.0, "rationale": "Establish the journey"}
  ]
}
```

`scriptRevisionId` is optional. Unknown fields are rejected, including unsupported
track/effect fields. Positions must equal zero-based array indices. Empty
`operationId` values receive generated IDs. Keep the same ID when trimming or
reordering a cut; give each new cut, split, or repeated-source use its own ID.
IDs are unique within each revision and may persist across revisions. A diff
returns each changed cut's identity, before/after values, and change kinds
(`added`, `omitted`, `order`, `source`, `range`, `rationale`, `audio`).

Ranges are finite seconds on the source clock, inclusive in / exclusive out.
Source reuse and overlapping ranges are legal and reported as advisory warnings.
No frame snapping occurs at plan persistence: retain exact measured duration for
an endpoint at source end. Rendering passes decimal seconds without millisecond
rounding; decoding selects source frames and the preview resamples to its
30000/1001 frame grid. Its encoded frame/audio packet boundaries are not proof
that a model's decimal locator is a frame-accurate source boundary.

Creation always saves an inactive immutable draft, including finite invalid
ranges so the validation report can explain and recover the work. Nonfinite
ranges and duplicate cut IDs are refused before saving. `validate` returns
`renderable` and findings; blocking invalid/unknown-duration/out-of-media ranges
and unavailable originals prevent activation/rendering. A refused activation
leaves the draft available. Older revisions and desktop drafts are retained.

`--expected-base` selects the immutable parent (any same-project draft), while
`--expected-active` independently guards the current active plan. Both are
required for creation; use the literal `none` to assert absence. Activation
requires `--expected-active` too. Guard checks and writes share one immediate
SQLite transaction, so concurrent clients cannot silently replace a newer active
plan. Stale writes report `edit_plan_stale_revision`; failed validation reports
`edit_plan_validation_failed`. Preview never activates and returns the addressed
revision ID plus preview metadata. Progress events go to stderr.

Revision-bound review feedback remains a follow-up slice under #112.

### Independent source audio

An omitted `audio` field preserves legacy linked source sound. An explicit
`{"mode":"silent"}` suppresses that picture cut's linked audio, for example on
B-roll covering retained dialogue. To give source sound independent handles:

```json
{
  "mode": "source",
  "version": 1,
  "assetId": "audio-source-asset-id",
  "sourceInSeconds": 0.5,
  "sourceOutSeconds": 2.5,
  "timelineStartSeconds": 0.5,
  "syncOffsetSeconds": 0,
  "gainDb": -6,
  "fadeInSeconds": 0,
  "fadeOutSeconds": 0.05
}
```

Place this object in a cut's `audio` field. Picture positions still define one
contiguous hard-cut track. Audio placement uses movie seconds from zero and
must stay inside that track's requested duration. Audio may start before its
picture (J cut), end after it (L cut), or cover other silent imagery. Its source
must be a clip in the same project with sufficient verified audio-stream handles
in the chosen review representation; unknown audio duration requires review.

During coincident imagery from that source, `syncOffsetSeconds` is the audio
source clock minus the picture source clock. Zero keeps sync; a nonzero offset
must match the explicitly requested placement. Incompatible repeated-source
mappings are rejected. Version, finite intervals, source handles, stream presence,
placement, sync, gain and fade limits are checked before activation or rendering.

At most two audible source intervals may overlap. Mixing is a deterministic sum
at the explicit gains, with no normalization or ducking; choose sufficient gain
headroom to avoid clipping. Gains default to 0 dB and must be -60..=0. Fades
default to zero, must be at most 250 ms each, and must fit the interval. They do
not silently expand to cover speech. Explicit-audio previews are bounded to six
hours, use stereo 48 kHz samples, and decode the full result before publication.
Every picture cut must occupy at least one preview frame.
Source in points round inward up to a whole 48 kHz sample; out points round
inward down, and movie placement rounds to the nearest sample. Preview picture
boundaries round on the cumulative 30000/1001 frame grid, avoiding per-cut drift;
output resampling may hold the last accepted source frame for the fractional
frame boundary. This creates no additional source-frame evidence.

The immutable operation uses `editPlanRevisionCreated` payload version 2 whenever
any cut declares audio; older clients refuse that version. Schema 50 preserves
legacy rows with absent audio. Audio-only changes appear as `audio` in the normal
revision diff. Creating or previewing the draft never activates it. A per-revision
render claim prevents concurrent writers, and failed/cancelled attempts never
replace a completed preview. OTIO export refuses any explicit audio mode with
`otio_unsupported_audio` before creating an output or export record; finishing
acceptance under #128 is separate.

### Bounded source inspection

`edit-plan inspect --input /absolute/inspection.json` explicitly generates or
reuses local source-window evidence without activating or changing a plan.
See [source inspection](edit-plan-inspection.md) for the JSON contract, exact
representation PTS requests, mapping precision, sound policy and limitations.
