# Proxy generation

The macOS desktop, Linux CLI, and Ubuntu companion server generate review
proxies through the same core encoder policy. Originals remain read-only and
generated files continue to use the project-local `v3` cache contract because
resolution, codec, pixel format, orientation, audio, and seekability are
unchanged.

## Encoder policy

`auto` is the safe default. On Linux it runs a bounded synthetic probe before
using VAAPI; on macOS it retains the existing per-clip VideoToolbox-first
behavior. `software` is explicit and deterministic: it never opens a DRM render
node and always uses `libx264`. The CLI exposes the policy on single and batch
generation:

```bash
rcdvlog proxy generate --asset <id> --policy auto
rcdvlog proxy generate --asset <id> --policy software
rcdvlog proxy missing --project <id> --policy software
rcdvlog proxy probe --policy auto
rcdvlog proxy provenance --asset <id> --limit 20
rcdvlog proxy history
```

The companion server reads `RCDVLOG_PROXY_ENCODER_POLICY=auto|software`
(default `auto`). It reports the policy in configuration diagnostics.

### Linux capability probe

A render node, a readable `/dev/dri/renderD*` path, or an `h264_vaapi` line in
`ffmpeg -encoders` is not sufficient to claim hardware support. `proxy probe`
and the `auto` path first generate a bounded 0.2-second synthetic H.264 clip,
then run the actual VAAPI decode, device-filter-pool scale, and H.264 encode
recipe over it. The probe is cached once per process. Its JSON keeps these
facts separate:

- `devicePresent`: the node exists.
- `deviceAccessible`: this process can open it read/write.
- `ffmpegEncoderPresent`: the configured FFmpeg advertises the encoder.
- `initializationTested` / `usable`: a real bounded encode actually succeeded.
- `status`: one of `usable`, `device_unavailable`,
  `device_permission_denied`, `ffmpeg_encoder_unavailable`,
  `initialization_failed`, `not_requested`, or `unsupported_platform`. Each
  non-usable outcome has actionable `detail`.

Usable candidates are preferred in deterministic path order. `auto` still
retains the software target after a successful probe so an individual source
that VAAPI cannot decode or scale can fall back safely.

## Linux VAAPI recipe

The full-clip recipe uses one device for hard decode, the VAAPI filter pool, and
encoding:

- `-init_hw_device vaapi=va:<render-node>` and `-filter_hw_device va`.
- `-hwaccel vaapi -hwaccel_device <render-node> -hwaccel_output_format vaapi`.
- `scale_vaapi=w='min(1280,iw)':h=-2:format=nv12`.
- H.264 High profile through `h264_vaapi` at QP 23.
- Input display rotation overridden to 0 with `-noautorotate`, map the raw
  encoded orientation, map `0:v:0` and optional `0:a:0?`, AAC 128 kb/s,
  `+faststart`, and `-map_metadata -1`.

If the hardware process fails, or its output fails strict validation, only that
attempt's unique staging file is removed and the standard software recipe is
retried. The previous registered output is never replaced until a complete
attempt validates and commits.

## Validation and provenance

Before atomic registration the staged output must be H.264, at most 1280 px
wide, `yuv420p`, carry zero display rotation, remain within the source-duration
tolerance (at most 0.25 s), preserve source-audio presence as AAC, and decode
successfully through FFmpeg with strict error handling and both video and audio
mapped. Where stream metadata is available, source/output video frame counts
must match, video durations must agree within 0.1 s, and audio duration and its
relative start must agree within 0.1 s. Timestamp passthrough avoids implicit
frame-rate conversion. These checks do not certify every frame's content. Validation runs
inside the existing `RepresentationWrite` claim.

Every completed or failed transcode attempt writes a row to the project
catalog's `proxy_generation_provenance` table. SQLite rows retain the semantic
recipe and actual encoder, device, policy, elapsed timing, ordered attempts with
process/validation failures, fallback flag, bounded validation evidence, and
timestamp. Failed rows never change `assets.proxy_path`. `proxy provenance`
returns these rows newest first; `proxy history` returns encoder- and
worker-count-scoped throughput observations with an explicit scope note.

## macOS recipe

Up to four workers run concurrently. Each first attempts VideoToolbox decode and
H.264 encode while retaining FFmpeg's CPU `scale` filter. Local M3 testing found
`scale_vt` substantially reduced downscale fidelity on 4K 10-bit DJI HEVC
footage, so scaling is not moved to VideoToolbox. Video uses High profile and
constant quality 36. Audio remains AAC 128 kb/s, metadata is removed, and
fast-start is enabled.

If VideoToolbox is unavailable, busy, unsupported by the installed FFmpeg, or
rejects an input, the same operation removes the incomplete temporary output and
retries with `libx264 veryfast` at CRF 23. The final file is probed and atomically
committed only after the existing zero-display-rotation contract passes.

FFmpeg reports its output timestamp once per second. The desktop converts that
timestamp into a fraction of the source duration, applies the fraction to the
source-byte work weight, and includes the residual bytes in every ETA update.
This lets an in-flight long clip count down before it finishes. Timestamp
progress does not update persisted throughput; only a successfully completed
clip contributes its full bytes and worker elapsed time. If FFmpeg provides no
timestamp, the operation safely falls back to completion-only progress. A
VideoToolbox failure resets that clip's partial work before the software retry.

The Ubuntu companion server uses the same core policy: Linux `auto` may select
VAAPI with software fallback, and `software` remains available explicitly.
Actual encoder, attempt, fallback, and validation evidence is stored in the
server job result details (`proxyProvenance`) in canonical SQLite for both
successful and failed attempts; failed jobs never publish a representation.
Server throughput observations for representation jobs are recorded under the
encoder/worker-scoped key that actually succeeded (reported as
`rateHistoryKey`), while the public representation recipe remains the semantic
v3 proxy contract.

## 2026-08-03 M3 benchmark

The benchmark used an 8-core Apple M3 MacBook Air and read immutable originals
from a large real corpus (`$RCDVLOG_ACCEPTANCE_MEDIA_ROOT`), not from fixtures.
The corpus contained 273 files (98 GB); inspected files were 3840x2160, 59.94 fps, 10-bit HEVC with AAC audio.
Outputs were H.264 High, 1280x720, 59.94 fps, `yuv420p`, AAC, and zero display
rotation. Temporary outputs lived outside the source tree.

On a 20-second representative segment:

| Recipe | Wall time | Aggregate CPU | Output size | VMAF |
| --- | ---: | ---: | ---: | ---: |
| Existing `libx264 veryfast` CRF 23 | 23.71 s | 492% | 11.34 MB | 91.41 |
| VideoToolbox + CPU scale, quality 35 | 7.63 s | 203% | 12.42 MB | 89.80 |
| VideoToolbox + CPU scale, quality 36 | 7.27 s | 208% | 14.07 MB | 91.93 |
| VideoToolbox + CPU scale, quality 38 | 7.35 s | 206% | 15.77 MB | 93.54 |
| VideoToolbox + `scale_vt`, quality 40 | 5.83 s | 55% | 13.31 MB | 74.42 |

Quality 36 was selected because it matched the existing objective fidelity while
remaining 3.3 times faster. A complete second 13.83-second clip improved from
17.82 seconds to 4.95 seconds. A 30-second seek into the corpus's 16 GB clip was
more storage/content constrained but still improved from 36.38 seconds to 22.77
seconds, while aggregate CPU fell from about 497% to 112%.

The macOS ETA history key family is
`proxy.h264-720p.orientation-v3.macos-videotoolbox-wN-v2` and
`proxy.h264-720p.orientation-v3.macos-software-wN-v2`, where `N` is the
actual worker count from one through four. Linux histories are separated by the
encoder that actually succeeded:
`proxy.h264-720p.orientation-v3.linux-vaapi-qp23-wN-v1` and
`proxy.h264-720p.orientation-v3.linux-software-wN-v1`. Old software-rate samples
and rates learned under a different encoder or concurrency are intentionally not
reused. A rate describes only successful work on this host/catalog and its
observed corpus mix; it is not a cross-corpus benchmark.

## Linux limitations and acceptance

The probe certifies only a bounded synthetic H.264 source plus actual encoder
initialization. It does not certify every camera codec, 10-bit profile, color
transfer, resolution, concurrent session count, or driver revision. Some valid
inputs may therefore cause the hardware attempt to fail and use the software
fallback. When no render node exists or initializes, hardware acceptance is
explicitly unavailable and the probe reports why rather than pretending the
device file is enough. Real hardware smoke coverage should use generated media
and mark unavailable hardware; deterministic tests force both process failure
and invalid hardware output to prove cleanup and fallback.

## Worker-count benchmark

Parallelism used a fixed eight-clip workload from the same corpus: eight distinct
4K/59.94 HEVC originals totaling 125.38 seconds. After a first sweep exposed a
large macOS source-cache effect, the key counts were rerun with all source files
equally warm. Four workers were the stable throughput peak:

| Workers | Wall time | Aggregate CPU | Source-time throughput |
| ---: | ---: | ---: | ---: |
| 1 | 46.53 s | 212% | 2.69x |
| 2 | 29.97 s | 406% | 4.18x |
| 3 | 33.26 s | 557% | 3.77x |
| 4 | 25.10 s | 553% | 4.99x |
| 6 | 31.05 s | 465% | 4.04x |
| 8 | 32.91 s | 626% | 3.81x |

Six and eight workers regress because the VideoToolbox sessions and CPU scalers
contend. Four is therefore the macOS default, capped by the number of pending
assets. A direct single-proxy request still uses and reports one worker. The
server remains at one proxy worker.

The registered proxy points to its successful provenance row. A later failed
attempt remains visible as latest attempt evidence without changing the
registered encoder claim; downloaded outputs clear local generation provenance.
Publication preserves a hard link to an existing output until the new staging
file and SQLite association commit. Server settings identities describe the
shared semantic policy and carry actual encoder recipes in durable job results.
In-flight server cancellation kills encoding/validation children and suppresses
fallback. Encoder rate histories exclude time spent in earlier failed attempts;
total elapsed time and failures remain in transaction provenance. macOS uses
new separate software/VideoToolbox history keys instead of its old mixed key.
