# Task 14: Web Library, Project Browse, and Add-to-Project

**Status:** Planned
**Depends on:** Tasks 5, 8, and 13
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
- [ ] No project create/delete or original-media mutation is exposed.
- [ ] Browser back/forward restores useful query/page state.
- [ ] Wide and narrow Chrome layouts, keyboard selection, focus, empty, and error states are visually verified.

## Verification

Add server query/command tests and Chrome Playwright flows for browse, filter, add, duplicate add, offline Mac, and availability states. Include a scale fixture with multiple projects and shared media objects.

## Handoff notes

Task 15 builds review on the project asset browser. Keep media details and availability components reusable without letting the frontend infer domain identity.
