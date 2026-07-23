# Handoff After Task 6

**Date:** 2026-07-23
**Status:** Task 6 implemented; automated tests pass; manual Chrome/Ubuntu/NAS acceptance not run
**Next:** Task 7 — Operation-Backed Project Mutations

## Migration identity

Control catalog schema version: **5**

Migration 5 is `representation_store_media_jobs` with immutable source identity `server-control/0005-representation-store-media-jobs/20260723`. It adds `representations`, `representation_locations`, and the active-representation-job uniqueness index. Migrations 1–4 are unchanged. The upgrade test covers migration 4→5 preservation of verified media and durable jobs.

## Public server-core surface

- `Representation`, `RepresentationKind`, and `RepresentationState`
- `RepresentationRequest` and `RepresentationRequestResult`
- `RepresentationService` and stable `RepresentationError::code()` values
- `RepresentationJobHandler` and combined `ServerJobHandler`
- `PROXY_RECIPE_KEY` and `THUMBNAIL_RECIPE_KEY`
- `ControlCatalog::representations`

## Runtime behavior

The server scheduler now dispatches both scan and representation jobs. Representation requests are durable and retryable, duplicate active requests converge, generated files commit atomically below the derived root, and file-before-database restart recovery is idempotent. Server-local content delivery implements streaming full/single-range responses without filesystem paths in URLs.

See [REPRESENTATIONS_AND_STREAMING.md](REPRESENTATIONS_AND_STREAMING.md) for the detailed contract.

## Manual gaps

- Real Chrome seek/playback smoke test
- Ubuntu shared-derived-root deployment
- Read-only NFS/SMB original mounts under disconnect/reconnect
- NAS-scale 4K throughput and file-handle profiling
- Authentication and HTTPS, owned by Task 9
