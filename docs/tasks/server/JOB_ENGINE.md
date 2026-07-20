# Durable Server Job Engine

Control-catalog migration 3 (`durable_server_job_engine`) stores server-local
jobs, attempts, dependencies, leases, rate history, and append-only events.
Migrations 1 and 2 are unchanged. Jobs move from `queued` to `running`, then
to `succeeded`, `retry_wait`, `failed`, or `canceled`; elapsed retry waits are
promoted back to `queued` during claims.

`JobEngine` is the only repository API. Claiming runs in an immediate SQLite
transaction and creates one lease-tokened attempt. Heartbeat, progress, cancel
acknowledgement, failure, and success require that exact unexpired token and
attempt. A successful handler calls `complete`: its output-registration closure,
fresh cancellation check, terminal state, attempt record, and event
commit together. Event replay is durable and sequence-based.

`JobEngine::get_job` is the monitoring and output-reuse contract. It opens the
existing catalog read-only and returns a `JobSnapshot` with the definition,
state, attempt and progress counters, availability/cancellation timestamps,
safe structured error, lifecycle timestamps, and any successful immutable
output identity. Snapshots deliberately omit lease owner/token fields and do
not run migrations or mutate state. Enqueuing an idempotent duplicate returns
the original definition ID, which can then be resolved through this snapshot to
reuse its succeeded output identity.

Only successful item-completion observations update throughput history;
terminal completion does not manufacture a rate sample. Payloads are bounded,
versioned JSON objects. `JobRequirements` is a concrete
versioned DTO with original/derived-write flags and a closed server-capability
enum (original read, derived write, ffmpeg, ffprobe, exiftool);
optional project, media-object, root, and representation IDs are explicit DTO
fields. Command/program/shell payload keys are rejected. Error/event detail
objects are bounded and reject command-shaped data, paths, and secret-like
strings. Idempotency is scoped to the target server node and job type.

Every `report_progress` and `record_item_completion` call supplies a bounded
`ProgressEstimateInput`: the actual worker count and one work weight for every
remaining task. The engine rejects zero or class-over-cap workers, more workers
than remaining tasks, a missing/extra task weight, or a sum that differs from
durable remaining work. It restores durable item/work counters into
`BatchEstimator` and gives it the individual remaining weights, so multi-worker
ETA uses the slowest shard rather than an aggregate-work shortcut. This input is
transient; SQLite counters and rate history remain canonical.

Handlers return categorized bounded failures and may use `commit_result` to
register output in the same SQLite transaction as success. The scheduler permits zero or one proxy worker, zero or one full-hash worker,
and zero through two lightweight workers, with at least one worker enabled.
Leases are bounded to 10 ms through one hour, scheduler polling to one
minute, and durable cancellation polling to one second (200 ms by default).
Heartbeat updates only the lease row; it does not append events. Shutdown stops
claims, cancels running handler
tokens, and treats an interrupted handler as retryable `server_shutdown` unless
a durable user cancellation already wins. It drains successful result commits,
then returns. `CancellableChild` specifies a
generic graceful-stop then forced-stop contract; media process launching belongs
to Tasks 5 and 6.

Deferred handlers: scans, hashes, metadata probe, thumbnails, proxies,
representation registration, Mac queueing, remote routing, web administration,
and distributed competitive workers.
