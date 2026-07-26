# Task 14: Web Library, Project Browse, and Add-to-Project

**Status:** Planned
**Depends on:** Tasks 5, 6, 8, and 13
**Unblocks:** Task 15

## Objective

Build the web experience for browsing the global NAS library and replicated projects, inspecting identity/availability, and explicitly adding library media to an existing project.

## User outcome

In Chrome, the user can find archived footage, understand whether it is verified and where it is accessible, inspect project membership, and add selected media to an existing desktop-created project.

## Scope

- Add paginated server APIs/client methods for library and project browse.
- Add search/filter/sort over filename, camera, current labels/notes where project-scoped, verification, storage root, availability, and project membership.
- Add library grid/list with quality-1 thumbnails, identity state, and technical metadata.
- Add project list and project asset browser.
- Add media details with capability-oriented availability and optional diagnostic paths behind details.
- Add single and bounded batch “Add to project” command with preview and idempotency.
- Create server-origin project asset operations and show synchronization status to the Mac.
- Handle media that is server-only, Mac-only, both, provisional, mismatched, offline, or missing a proxy.
- Add deterministic cursor pagination and browser history/query-state behavior.

## Out of scope

- Creating/deleting projects
- Editing annotations
- Proxy playback
- Original uploads or filesystem relink
- Full-text/semantic search
- Automatically adding scanned media to a project

## Query surface

Task 3 shipped `StorageLibrary::list_media` with a 1–200 limit, a single optional
verification-state filter, and `(createdAt DESC, mediaObjectId ASC)` ordering. Every filter
and sort key this task adds is a schema change, not a query-builder change:

- Each supported sort order needs its own composite index and a cursor tuple matching that
  order exactly. A cursor is only valid for the sort/filter combination that produced it.
- An unsupported sort/filter combination is rejected with `invalid_query` rather than served
  by a full scan.
- Availability is a derived projection over observation generations, not a column. Either
  materialize an indexed availability summary per `(mediaObjectId, nodeId)` alongside the
  observation-completion transaction, or restrict availability to a post-filter over an
  already-bounded page and say so in the API contract. Do not filter availability with a
  correlated subquery over `observation_generations`.
- Current notes and labels live in the project replica, not the control catalog, and SQLite
  cannot join across the two databases. Annotation search is therefore a project-scoped query
  served by the replica; the global library search covers only global technical facts. The
  two are separate endpoints with separate cursors and must not be merged into one result set.

## Add-to-project command rule

Global library membership dedupe cannot read `project_media_memberships`. That table is a
repairable projection that legitimately lags a project commit and stays `pending` when the
referenced media row has not arrived, so it can report "not a member" for an asset that is
already committed in the replica.

The command therefore:

1. Opens the project replica and resolves existing membership from the durable operation log
   or its materialized projection — the replica is the only dedupe authority.
2. Emits `projectAssetAdded` with a self-contained metadata projection built from the media
   object's global technical facts and no absolute path, then `projectAssetMediaLinked`
   carrying `mediaObjectId` and the current `verificationState`. Both are frozen payload
   version 1 operations from Task 7; this task adds no new operation type.
3. Repairs the control-catalog membership projection after the replica commit.

Verification gating:

| Media state | Behavior |
| --- | --- |
| `verified` | Add normally. |
| `provisional` or `unverified` | Add only with explicit acknowledgement; the link records the state at add time and no representation may be requested until full-hash verification completes. |
| `mismatch` | Blocked with an actionable diagnostic; the user resolves identity first. |

A later verification transition emits a new `projectAssetMediaLinked` for the same stable
`projectAssetId`. It never rewrites or collapses the existing project asset.

## UX requirements

- Default library view identifies verified versus provisional media without alarming normal users.
- “Add to project” lists only existing replicated projects.
- Preview reports selected count, already-member count, provisional/mismatched items, and remote-only consequences.
- A successful add shows server commit and whether the Mac has synchronized it yet.
- Absolute paths stay in an expandable diagnostics area. Primary UI shows storage-root name and node capability.
- Empty/offline/error states suggest relevant safe actions such as scan, generate proxy, or reconnect a mount.

## Design requirements

1. Lists are cursor-paginated; no endpoint or React state loads the entire archive by default.
2. Sort order is deterministic across pages.
3. Query parameters are validated and indexed; simple current lexical search is sufficient.
4. Batch add is bounded and uses an idempotency key.
5. Each new project asset receives one stable `projectAssetId` and self-contained metadata projection.
6. Project annotations initialize empty and are never inherited from another project's membership.
7. Provisional cross-location matches cannot silently collapse existing project assets.
8. Adding media does not require the Mac or its NAS binding to be online.
9. Repeating an add returns already-member/skipped results rather than duplicating membership.
10. Library technical facts remain global; project removal and labels remain project-scoped.
11. Thumbnail requests remain authenticated and do not expose filesystem paths.
12. Keyboard navigation, focus after add, and batch selection are accessible.
13. Project membership commits in the project replica first; the global-library membership index is an idempotent projection that can be repaired after interruption.
14. Duplicate detection reads the project replica, never the membership projection.
15. Media without a verified full hash has no thumbnail, because representations require verified identity. That case renders identity state and technical facts, not a broken image or an infinite skeleton.
16. Every new endpoint updates the committed OpenAPI document and generated TypeScript client; `pnpm check:openapi` fails on drift.
17. WebSocket messages are hints that trigger a canonical refetch. No list or detail view derives its contents from an event payload.

## Likely implementation areas

- `core/rcdvlog-server` library/project query and add command services
- `apps/server` authenticated library/project endpoints
- `packages/project-client`
- `apps/web` library, project, detail, filter, and selection views
- `packages/review-ui` thumbnail/media badges where shared
- Playwright and API integration tests

## Acceptance criteria

- [ ] Chrome browses deterministic pages of thousands of synthetic library records without a full-data fetch.
- [ ] Search/filter/sort combinations return correct stable results.
- [ ] Verification and node-relative availability are accurately represented.
- [ ] One media object may show membership in multiple projects without sharing annotations.
- [ ] Single and bounded batch add create server-origin project operations.
- [ ] Repeated add requests do not duplicate project assets.
- [ ] An injected crash after project commit but before library projection repairs the membership index on retry/startup.
- [ ] A server-only asset appears in the project immediately and later synchronizes to the offline/reconnected Mac.
- [ ] An added asset with no Mac binding shows remote-only rather than missing everywhere.
- [ ] Provisional/mismatch states require explicit acknowledgement or block unsafe add according to the documented command rule.
- [ ] An add issued while the membership projection is stale or `pending` still detects existing membership from the replica and does not create a second project asset.
- [ ] Unverified media renders identity and technical facts with no thumbnail request and no broken-image state.
- [ ] An unsupported sort/filter combination and a cursor reused across a different sort return `invalid_query` rather than a scan or a wrong page.
- [ ] Annotation search returns project-scoped results and global library search returns global facts; neither claims to cover the other.
- [ ] Committed OpenAPI and generated client match the Rust contract.
- [ ] No project create/delete or original-media mutation is exposed.
- [ ] Browser back/forward restores useful query/page state.
- [ ] Wide and narrow Chrome layouts, keyboard selection, focus, empty, and error states are visually verified.

## Verification

Add server query/command tests and Chrome Playwright flows for browse, filter, add, duplicate add, offline Mac, and availability states. Include a scale fixture with multiple projects and shared media objects.

## Handoff notes

Task 15 builds review on the project asset browser. Keep media details and availability components reusable without letting the frontend infer domain identity.
