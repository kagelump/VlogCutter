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

VlogCutter persists separate histories in each project catalog using versioned task keys such as `proxy.h264-720p.v1` and `thumbnail.q1.v1`. History uses a bounded eight-sample exponential window so recent hardware, codec, storage, or service behavior gradually replaces older measurements. Successful observations update speed; failures advance completion without poisoning throughput.
