# Batch estimation library

`core/batch-estimator` is a domain-independent Rust library for progress and ETA calculation across weighted batch work.

The caller supplies:

- A work weight for every task. Proxy and thumbnail generation use source bytes; VLM work could use frames, tokens, bytes, or a calibrated composite cost.
- A worker count. macOS missing-proxy batches use up to four workers, while a single proxy, thumbnails, and the server/non-macOS proxy path use one. A cloud VLM caller can supply its actual shard concurrency.
- Optional persisted `RateHistory`, expressed as work units per worker-second.
- Each completed task's weight and elapsed time on its worker.
- Optional partial work inside active tasks. The caller passes residual task
  weights plus the aggregate partial work for display; partial work reduces ETA
  but never becomes a throughput sample.

ETA is not calculated as `remaining work / rate / workers`. Remaining tasks are sorted by weight and assigned to the least-loaded worker; the slowest estimated shard determines completion time. This avoids overly optimistic estimates when a few large files dominate a multi-worker queue.

```rust
use batch_estimator::{BatchEstimator, RateHistory};
use std::time::Duration;

let weights = vec![900, 800, 700, 600];
let mut batch = BatchEstimator::new(&weights, 2, RateHistory::default());
batch.record_completion(900, Duration::from_secs(9));
let estimate = batch.estimate(&weights[1..]);
```

Desktop proxy generation reads FFmpeg's output timestamp once per second and
maps the completed duration fraction onto each clip's source-byte weight. This
keeps the existing byte-rate model while preventing a long active clip from
remaining at its full weight until process exit. Completed item counts advance
only after validation and atomic commit.

When a caller restores a durable aggregate job rather than a complete original
task list, `BatchEstimator::with_explicit_progress` accepts its persisted total
item/work counters plus current completed counters. The caller must still pass
the exact individual weights of every remaining task to `estimate`; this keeps
parallel ETA based on the slowest shard. The server job engine validates that
those weights exactly match durable remaining item and work totals before using
the constructor.

Desktop batches persist separate histories in each project catalog using versioned task keys such as `proxy.h264-720p.orientation-v3.macos-videotoolbox-q36-w4-v1` and `thumbnail.q1.orientation-v2`. The macOS proxy key includes the actual worker count (`w1` through `w4`) because per-worker VideoToolbox throughput changes under contention; a software fallback that completes under that attempt remains part of the observed Mac rate for that concurrency. A smaller batch is capped by the number of pending assets, and a single-proxy call always uses the `w1` history. The companion server persists its `proxy.h264-720p.orientation-v3` history in the local control catalog, scoped to the stable server node and job task key, and never treats a Mac observation as Linux throughput. Server scan/hash handlers use their own versioned task identities.

The server scheduler caps proxy work at one worker, full-hash work at one worker, and lightweight work at two workers by default. Every progress update supplies the actual worker count and exact remaining item weights; the durable job engine rejects inconsistent counts, excessive concurrency, or aggregate totals that do not match durable progress. A restored job uses `with_explicit_progress` plus the remaining individual weights, so its ETA remains a slowest-shard estimate after restart.

Server representation timings are not keyed by the semantic representation recipe
alone. A successful media observation is qualified by the exact configured
FFmpeg/FFprobe executable identities, host/build/device facts, actual selected
encoder and execution recipe, and worker/thread configuration. The context is
persisted with the current leased attempt and with the rate row. Attempt
contexts are also fenced by a process-session key, so a lease left behind by a
restarted server cannot keep advertising the old ETA. Queued, waiting,
interrupted, missing, or stale contexts yield `None` ETA and explicit
`uncertain_*` confidence rather than reusing generic legacy proxy history. A
hardware-to-software fallback records only the successful software attempt and
never lets hardware throughput estimate software progress. Non-media full-hash
and lightweight estimates retain their existing recipe-scoped histories.

History uses a bounded eight-sample exponential window so recent hardware, codec, storage, or service behavior gradually replaces older measurements. Successful item observations update speed; failures and cancellations advance durable work state without poisoning throughput. Change the versioned task key whenever recipe or cost semantics make older measurements incomparable.
