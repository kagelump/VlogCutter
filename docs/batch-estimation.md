# Batch estimation library

`core/batch-estimator` is a domain-independent Rust library for progress and ETA calculation across weighted batch work.

The caller supplies:

- A work weight for every task. Proxy and thumbnail generation use source bytes; VLM work could use frames, tokens, bytes, or a calibrated composite cost.
- A worker count. Local proxy and thumbnail generation currently use one worker. A cloud VLM caller can supply its actual shard concurrency.
- Optional persisted `RateHistory`, expressed as work units per worker-second.
- Each completed task's weight and elapsed time on its worker.

ETA is not calculated as `remaining work / rate / workers`. Remaining tasks are sorted by weight and assigned to the least-loaded worker; the slowest estimated shard determines completion time. This avoids overly optimistic estimates when a few large files dominate a multi-worker queue.

```rust
use batch_estimator::{BatchEstimator, RateHistory};
use std::time::Duration;

let weights = vec![900, 800, 700, 600];
let mut batch = BatchEstimator::new(&weights, 2, RateHistory::default());
batch.record_completion(900, Duration::from_secs(9));
let estimate = batch.estimate(&weights[1..]);
```

When a caller restores a durable aggregate job rather than a complete original
task list, `BatchEstimator::with_explicit_progress` accepts its persisted total
item/work counters plus current completed counters. The caller must still pass
the exact individual weights of every remaining task to `estimate`; this keeps
parallel ETA based on the slowest shard. The server job engine validates that
those weights exactly match durable remaining item and work totals before using
the constructor.

Desktop batches persist separate histories in each project catalog using versioned task keys such as `proxy.h264-720p.orientation-v3` and `thumbnail.q1.orientation-v2`. The companion server persists rate history in its local control catalog, scoped to the stable server node and job task key; it never treats a Mac observation as Linux throughput. Current shared representation recipes use the same two keys, and server scan/hash handlers use their own versioned task identities.

The server scheduler caps proxy work at one worker, full-hash work at one worker, and lightweight work at two workers by default. Every progress update supplies the actual worker count and exact remaining item weights; the durable job engine rejects inconsistent counts, excessive concurrency, or aggregate totals that do not match durable progress. A restored job uses `with_explicit_progress` plus the remaining individual weights, so its ETA remains a slowest-shard estimate after restart.

History uses a bounded eight-sample exponential window so recent hardware, codec, storage, or service behavior gradually replaces older measurements. Successful item observations update speed; failures and cancellations advance durable work state without poisoning throughput. Change the versioned task key whenever recipe or cost semantics make older measurements incomparable.
