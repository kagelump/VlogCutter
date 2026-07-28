# HTTP API contract

The Rust server owns the OpenAPI document at `GET /api/openapi.json`; [`packages/project-client/openapi.json`](../../packages/project-client/openapi.json) is its drift-checked snapshot. Browser code uses `packages/project-client` rather than handwritten fetch calls. Agents should prefer the stable `rcdvlog server ...` JSON commands instead of constructing generic requests.

## Boundaries

- `GET /api/v1/health`, `/api/v1/ready`, and `/api/v1/version` are non-secret status routes. Liveness is cheap; readiness performs bounded database/storage/tool checks.
- `/api/v1/auth/bootstrap/exchange` and `/api/v1/auth/devices/exchange` are bounded one-time credential exchanges. Bootstrap creation has no remote route.
- Browser product/admin routes require the secure session cookie and same-origin CSRF on mutations.
- Internal baseline/reconcile/representation-completion routes and ordinary authenticated machine routes require a paired-device bearer credential.
- `/api/v1/ws/events` requires authentication and a same-origin WebSocket handshake. Events are invalidation hints, not canonical state.
- `/api/v1/media/{representationId}/content` serves only derived representations through opaque IDs and authenticated full/single-range `GET`/`HEAD`; originals are never served.

Resource groups are auth, projects/project assets, library, representations/media, internal baseline/reconcile, and admin storage/scans/schedules/jobs/sync/backups/audit. The checked OpenAPI document is the exhaustive route/method/schema source.

## Contract rules

JSON uses `camelCase`, stable opaque IDs, explicit versions, and bounded collections. List limits are 1–200; callers follow returned cursors rather than inventing offsets. Sync operations have separate 500-operation/4 MiB bounds. Mutations that may be retried accept idempotency identity and return a conflict when the same key is reused for different input.

Every response carries `X-Request-ID`; JSON results/errors include the same request identity where applicable. Client-visible failures use the checked-in stable registry in [`error_codes.rs`](../../core/rcdvlog-server/src/error_codes.rs), including auth/security, compatibility, mount, media, job, backup, sync, and internal categories. Branch on the code, not the human message.

Host, Origin, trusted-forwarded-header, CSRF, body-size, path-containment, and authentication checks apply before domain mutation. CORS is same-origin. Tokens never appear in query parameters. Filesystem paths are permitted only in explicit local administration projections and never in sync-safe project/media DTOs.

## Change workflow

1. Change the owning Rust DTO/domain and API adapter.
2. Update the Rust-owned OpenAPI generator and route tests.
3. Run `pnpm generate:openapi` intentionally.
4. Update the typed client and consumers.
5. Run `pnpm check:openapi`, server tests, frontend checks, and relevant browser tests.

Do not edit the checked OpenAPI JSON by hand. The protocol-v1 baseline/reconcile schemas under [`docs/tasks/server/schemas`](../tasks/server/schemas/) are frozen compatibility artifacts and follow a new-version migration process rather than OpenAPI regeneration.
