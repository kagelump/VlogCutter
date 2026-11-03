# Change recipes

Relocated verbatim from the repository-root `AGENTS.md` section 14 so the root file can stay a short index (issue #260).

## Add a new domain operation

1. Add or change canonical types and validation in `rcdvlog-core`.
2. Add core unit tests, including invalid input and missing/offline state.
3. Add a thin Tauri command and register it.
4. Mirror DTOs in `types.ts` and expose the call only through `api.ts`.
5. Add UI behavior, accessible labels, keyboard access, error state, and progress if relevant.
6. Add a CLI command when agents need the operation; preserve JSON envelopes and stable error codes.
7. Add process-level integration coverage for cross-boundary behavior.
8. Update the relevant document under `docs/` and this file if architecture or workflow changed.

## Add a long-running batch task

1. Choose a meaningful work unit and versioned task key.
2. Declare the actual worker count; never assume all jobs are single-worker.
3. Use `BatchEstimator` and persisted per-project rate history.
4. Emit initial progress, each completed asset/result, failures, worker count, and ETA.
5. Commit successes incrementally so interruption does not discard completed work.
6. Do not let failed observations poison speed history.
7. Update assets in the UI as each item completes.
8. Test heterogeneous task sizes, worker sharding, no-history estimation, and failures.

## Add or change project settings

1. Update Rust `DesktopSettings` with a Serde default so older settings files remain valid.
2. Update the TypeScript mirror and the harness settings fixture (`apps/desktop/src/test/harness/state.ts`), which the browser preview reads too.
3. Persist atomically where practical and test a missing/older/corrupt settings file.
4. Expose the control in global settings, not per-project UI, unless the setting truly belongs in the catalog.

## Change media generation settings

1. Treat outputs as a new representation version.
2. Change cache/task keys so stale files and throughput history are not silently reused.
3. Update docs, tool checks, tests, and offline fallback behavior.
4. Test representative Sony/DJI/iPhone/Insta360 H.264/H.265 media, not only synthetic MPEG-4 fixtures.
