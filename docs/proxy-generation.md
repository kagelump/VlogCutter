# Proxy generation

The macOS desktop and CLI generate review proxies with a hardware-first FFmpeg
recipe. Originals remain read-only and generated files continue to use the
project-local `v3` cache contract because resolution, codec, pixel format,
orientation, audio, and seekability are unchanged.

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

The Ubuntu companion server retains its software recipe because VideoToolbox is
macOS-specific.

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
`proxy.h264-720p.orientation-v3.macos-videotoolbox-q36-wN-v1`, where `N` is the
actual worker count from one through four. Old software-rate samples and rates
learned under a different concurrency are intentionally not reused.

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
