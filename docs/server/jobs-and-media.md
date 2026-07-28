# Durable jobs, representations, and streaming

Server work is persisted in the local control catalog. Jobs move through `queued`, `running`, `succeeded`, `retry_wait`, `failed`, or `canceled`, with durable attempts, dependencies, leases, progress, safe errors, and replayable events. Restart recovers expired work; idempotency keys reuse equivalent requests. Retry is allowed only for eligible failures, and cancellation is durable and polled by handlers.

Default resource caps are one proxy worker, one full-hash worker, and up to two lightweight workers. At least one worker remains enabled. Media children are structured processes, not shell strings. Shutdown stops claims and converts interrupted handlers to retryable work unless user cancellation already won.

Scans, metadata probes, full hashes, thumbnails, proxies, representation registration, and backups all use the durable engine. Schedules store IANA time zones, coalesce missed runs, and do not overlap an active identical scope. Agents inspect/retry/cancel through `rcdvlog server ... jobs`; Chrome provides the same bounded projections and polling recovery when WebSockets are unavailable.

## Representation identity and recipes

Only full-hash-verified source media can create shared outputs. Identity includes source full BLAKE3 and length, kind, recipe key, and output settings, so two projects reuse one valid representation.

- `proxy.h264-720p.orientation-v3`: H.264, maximum 1280 pixels wide, `yuv420p`, zero display rotation, AAC 128 kb/s when audio exists, fast-start.
- `thumbnail.q1.orientation-v2`: JPEG at 20% duration, maximum 480 pixels wide.

Outputs are staged beside their deterministic final path, validated, atomically renamed, then published transactionally. Originals are read-only inputs. Failed/canceled attempts remove only their known temporary output. Existing compatible Mac outputs can be registered without synchronizing absolute paths.

Chrome streams only representations through opaque-ID URLs. `GET` and `HEAD` support full, bounded single, open-ended, and suffix byte ranges with immutable ETags; multiple ranges are rejected. The implementation streams a bounded file view and does not buffer an entire proxy. Original files are never HTTP content sources.

The Mac can download a compatible server proxy or thumbnail into its project-local cache. If the required recipe is Mac-only, the server records a node-targeted request and the desktop can fulfill it after reconnect. Original media never transfers in this epic.

Worker ETA uses source-byte weights and node-scoped, recipe-versioned rate history. Only successful item observations update throughput. See [batch-estimation.md](../batch-estimation.md).

Automated tests cover real generated media, atomic output, restart reconciliation, cancellation cleanup, RFC-style ranges, 10,000 queued jobs, and bounded worker claims. Chrome-over-LAN seek behavior, physical NAS loss, and target thermal/resource observations remain manual.
