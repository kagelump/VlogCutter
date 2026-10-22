# Agent waits and completion callbacks

Long work uses the existing durable job ID. `jobs wait` observes it inside one
CLI process; an agent harness can await that process instead of repeatedly
reading logs or issuing status commands. Callback subscriptions decouple the
job lifetime from a particular waiting process. These commands require the
same explicit server endpoint and paired credential as other server commands.

## Quiet waits

```bash
rcdvlog server --endpoint https://server.example jobs wait \
  --job job_example --wait-seconds 1800 --poll-interval-ms 1000
```

The command polls canonical server state internally and emits no progress or
heartbeat output. Success produces the ordinary single JSON stdout envelope
with `command: "server.jobs.wait"` and the final snapshot under `data.result`.
Failure produces the ordinary JSON error envelope on stderr and exit 1:

| Error code | Meaning |
| --- | --- |
| `job_failed` | The job reached terminal failure, including exhausted retries |
| `job_canceled` | Durable cancellation completed |
| `job_wait_timeout` | The observation deadline elapsed while the job was active |
| `server_unreachable` | Transport failed; job outcome is not established |

Terminal errors and observation timeout include the latest snapshot in
`error.details`. `queued`, `running` and `retry_wait` remain nonterminal.
`--wait-seconds 0` checks once. An already completed job returns immediately.
Observation never retries, cancels, enqueues, migrates or creates a local project
catalog. `--timeout-seconds` on `server` bounds each HTTP request separately;
a request already in progress can extend beyond the observation deadline.
Interrupting the waiting process leaves the server job unchanged.

## Durable subscriptions

```bash
rcdvlog server --endpoint https://server.example jobs callbacks register \
  --job job_example --target antarctica-editor
rcdvlog server --endpoint https://server.example jobs callbacks list \
  --target antarctica-editor --limit 50
```

A target is an opaque 1..128 character identifier containing ASCII letters,
digits, dot, underscore or hyphen. Registration is idempotent by job and target.
It subscribes to future terminal events and catches up with the latest event
if the job is already terminal. The registration/completion race is serialized
by SQLite. Success, failure and cancellation all enqueue deliveries; retry-wait
and ordinary progress do not. A later explicit job retry can produce a new
terminal event and a distinct delivery.

Control-catalog migration 11 adds subscriptions and delivery receipts. The
terminal job event and its deliveries commit atomically. Subscriptions and
receipts survive restart, including jobs completed before the migration.
`list` is a bounded read-only projection, with `nextBefore` pagination. It never
reveals the lease token.

## External delivery

```bash
rcdvlog server --endpoint https://server.example jobs callbacks deliver \
  --target antarctica-editor \
  --program /absolute/path/to/accept-completion \
  --arg=--workspace --arg /absolute/path/to/project \
  --wait-seconds 1800 --execution-timeout-seconds 60
```

This command waits quietly for **one** delivery for that target. It claims a
durable lease, starts the explicitly selected program locally, passes literal
argv without shell expansion, and writes one newline-terminated JSON envelope
to its stdin:

```json
{
  "schema": "job.completion.v1",
  "deliveryId": "delivery_...",
  "target": "antarctica-editor",
  "attempt": 1,
  "event": {
    "eventSequence": 42,
    "jobId": "job_example",
    "attemptNumber": 1,
    "eventType": "succeeded",
    "state": "succeeded",
    "details": {},
    "createdAt": "2026-10-22T00:00:00.000Z"
  }
}
```

The consumer acknowledges durable acceptance by exiting 0. Its stdout and
stderr are discarded to preserve the CLI JSON contract and avoid relaying
arbitrary consumer output; the consumer owns its detailed logs. Failures include
`callbackExitCode` and a bounded `callbackFailure` classification. The server bearer-token and credential-file
environment variables are removed from the child environment. Other inherited
environment variables retain their normal local-process semantics.

A nonzero exit, spawn/input failure, or execution timeout returns
`callback_failed` and releases the delivery for a later explicit attempt. The
local execution limit is 1..3300 seconds; the lease includes a 120-second
acknowledgement margin. On Unix, timeout kills the callback process group and
reaps the direct child. If the consumer process crashes or acknowledgement
cannot reach the server, lease expiry makes the delivery claimable again.
Empty-queue timeout returns `callback_wait_timeout` and starts no program.
An acknowledgement failure returns `callback_ack_failed`, with the delivery ID,
whether the consumer exited successfully, and `acknowledgement: "unknown"`.
Inspect the durable receipt before deciding whether to retry an external action.

Delivery is **at least once**, not exactly once. A consumer must deduplicate
its durable acceptance by `deliveryId`, which stays stable across retries.
Successful acknowledgements are idempotent, and expired/replaced lease tokens
cannot acknowledge another consumer's claim. A timeout or lost acknowledgement
does not prove the external side effect did not occur. The CLI never reruns the
consumer inside one invocation after an acknowledgement failure.

For a Codex integration, the external consumer maps its target to the owning
host and thread, persists the completion message, and starts or queues a turn
through that host's supported interface. It should return after durable
acceptance; a long agent turn should not keep the delivery lease occupied.
Codex host discovery, session ownership and side-effect deduplication belong to
that integration. VlogCutter stores no shell commands, Codex credentials or
session-control logic in job definitions or callbacks.

An external supervisor may run repeated `deliver` invocations to consume more
events. That loop needs no model inference. VlogCutter adds no resident callback
daemon or server-side arbitrary command executor.

## HTTP integration

All routes use existing authenticated admin protections and audit mutations:

| Method and path | Request |
| --- | --- |
| `POST /api/v1/admin/jobs/{jobId}/callbacks` | `{ "target": "editor" }` |
| `GET /api/v1/admin/job-callbacks?target=editor&limit=50` | Optional `before` cursor |
| `POST /api/v1/admin/job-callbacks/claim` | `{ "target": "editor", "leaseSeconds": 120 }` |
| `POST /api/v1/admin/job-callbacks/{deliveryId}/settle` | `{ "leaseToken": "...", "succeeded": true }` |

Claim returns `{ "claim": null }` when nothing is eligible, otherwise a
delivery plus a private `leaseToken`. Settle returns the receipt; `succeeded:
false` releases it for retry. A stale acknowledgement returns HTTP 409 with
stable error code `conflict`. Consumers performing remote side effects must
respect the lease and implement the same durable deduplication as CLI consumers.
