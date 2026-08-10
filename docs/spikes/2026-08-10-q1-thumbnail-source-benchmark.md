# Q1 thumbnail source benchmark

Issue: [#30](https://github.com/kagelump/VlogCutter/issues/30)  
Measured: 2026-08-10 on Apple Silicon (`arm64`), FFmpeg 7.1.3

The repository contains no decodable camera-media fixture, so this is a small
source-policy measurement rather than a camera-corpus benchmark. The input was
a generated six-second 3840x2160 60 fps HEVC Main clip (42 MiB). Its comparison
proxy was 1280x720 H.264 (556 KiB). Both used the production Q1 extraction
shape: seek to 20%, disable autorotation, decode one frame, scale to at most
480 px wide, and write JPEG quality 3.

Ten warm runs were measured for each source:

| Source | Median wall time | Range | Median user CPU | Relative wall time |
| --- | ---: | ---: | ---: | ---: |
| 4K/60 HEVC original | 0.30 s | 0.29–0.35 s | 1.63 s | 6.0× |
| 720p H.264 RawV3 proxy | 0.05 s | 0.05–0.06 s | 0.10 s | 1.0× |

Proxy-first materially reduced both elapsed decode time and CPU work in this
sample. The original decode also consumed substantially more aggregate CPU
than its wall time, so keeping one thumbnail worker remains the conservative
background policy while review or another import may be active. The thumbnail
batch estimator already captures completed real-project samples, allowing that
policy to be revisited with a representative camera corpus later.

This run did not claim an interactive-review or concurrent-import acceptance
result; those still require the native desktop with real project media. The
implementation keeps automatic work off the global busy state and runs only
one thumbnail worker, which are the relevant safeguards until that manual
measurement is available.
