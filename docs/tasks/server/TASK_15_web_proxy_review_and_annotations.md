# Task 15: Web Proxy Review and Annotations

**Status:** Implemented (2026-07-28)
**Depends on:** Tasks 6, 7, 11, 13, and 14
**Unblocks:** Task 19

## Objective

Turn the Chrome project browser into a useful secondary review surface with proxy playback and operation-backed editing of current annotations.

## User outcome

The user can review server-hosted proxies in Chrome, add or revise notes and free-form labels, rotate the review display, and remove or restore clips, with changes synchronizing back to the Mac.

## Scope

- Add authenticated proxy playback using byte-range media endpoints.
- Add bounded previous/current/next review navigation appropriate for Chrome.
- Add current quality-1 thumbnail poster/fallback.
- Add note revision, label add/remove, removed, and rotation commands with canonical responses.
- Add optimistic UI with base revisions and rollback/error handling.
- Add project search/filter integration for current notes, labels, filename, and camera.
- Add sync/pending/conflict indicators relevant to the selected asset.
- Add explicit states for missing proxy, queued generation, server-original unavailable, Mac-only original, and downloadable server proxy.
- Add keyboard and accessible custom playback/review controls without regressing desktop shortcuts.

## Out of scope

- Browser original-media playback
- Web chronology editing
- Note-history editor; history remains diagnostic-only
- New favorite/review-status fields
- Rough-cut inclusion, source ranges, timeline export, scripts, or edit plans
- Offline web editing

## Command semantics

The Task 7 payload version 1 operations are frozen. This task adds web adapters for them, not
new operation types, and must match their existing conflict rules:

- **Notes.** `noteRevisionCreated` is append-only and never fails on a stale base. The base
  revision travels with the command so the UI can tell the user another revision arrived, but
  the server does not reject the write: concurrent notes both survive and the greatest HLC key
  becomes active. Do not build an optimistic-concurrency retry loop around note saves.
- **Labels.** `labelRemoved` carries `observedAddTags` and removes only those tags, so an
  unobserved concurrent add wins. A browser sends the label; the server resolves the observed
  add-tags from its own materialized state inside the command transaction. The client must not
  invent add-tags from a possibly stale React cache, and the response returns the canonical
  label set.
- **Removed and rotation.** HLC last-write-wins registers. Rotation accepts only 0, 90, 180,
  and 270, and is persisted project state that synchronizes to the Mac — not a view-only
  display toggle.
- **Clock events.** Not editable from the web in this epic. An unresolved conflict is reported
  and directs the user to the desktop or CLI.

"Downloadable server proxy" is an availability state meaning the Mac can fetch this proxy
through Task 12. It is not a browser download control, and this task adds no such control.

## Design requirements

1. Chrome plays only a server-accessible compatible proxy in this epic.
2. Native media requests remain authenticated without bearer tokens in query strings. `<video>` and `<img>` rely on the same-origin HttpOnly session cookie, which is what keeps native range seeking working; a fetch-to-blob workaround defeats ranged delivery and is not acceptable.
3. Playback seeking uses range requests and does not fetch the entire proxy first.
4. Rotation applies to video content while controls remain correctly oriented.
5. Note saves create immutable revisions and include base revision.
6. Label controls issue explicit add/remove operations rather than replacing the full label array.
7. Removed and rotation commands return canonical materialized asset state.
8. Optimistic failure restores the prior canonical projection and shows the stable error/request ID.
9. Incoming WebSocket hints trigger canonical refetch/reconcile; they are not directly trusted as state.
10. Review resources stay bounded; navigating thousands of assets does not retain thousands of media elements or object URLs.
11. Text-field focus prevents global shortcuts, matching the desktop's safety principles.
12. Missing representations present safe actions and execution location, not generic playback errors.
13. The web UI never implies the browser works offline.
14. A media element that fails on authentication reports session expiry and offers re-authentication, distinct from a missing or pending representation.
15. Every new endpoint updates the committed OpenAPI document and generated TypeScript client.

## Suggested web keyboard contract

Reuse familiar keys where they do not conflict with browser behavior:

- `J`/`K` or arrows: next/previous asset
- Space: play/pause
- `L`: focus label input
- `N`: focus notes while consuming the original key
- `R`: remove/restore
- `/`: focus search
- Escape: cancel the current editor/modal

Visible help and `aria-keyshortcuts` must match the implemented contract.

## Likely implementation areas

- `core/rcdvlog-server` project query/command adapter
- `apps/server` annotation and media authorization routes
- `packages/project-client`
- `packages/review-ui`
- `apps/web` review workspace and state
- Existing desktop shared components only where extraction is safe
- Component and Playwright tests

## Acceptance criteria

- [ ] Chrome loads, plays, pauses, and seeks a current server proxy through authenticated range requests.
- [ ] Rotated clips display correctly while controls remain horizontal.
- [ ] Previous/current/next navigation keeps media resource use bounded.
- [ ] Notes create revisions and synchronize to the Mac after reconnect.
- [ ] Concurrent Mac/web note revisions both survive with the same deterministic active value.
- [ ] Label add/remove converges according to observed-remove semantics.
- [ ] Remove/restore and rotation converge and return canonical state.
- [ ] Optimistic command failure rolls back visibly without losing subsequent valid edits.
- [ ] Missing/offline/proxy-pending states show appropriate actions and node availability.
- [ ] No route serves original media to Chrome.
- [ ] Keyboard focus/shortcut behavior, notes newline/save behavior, and visible help are tested.
- [ ] Desktop shared-component extraction does not regress current desktop keyboard, focus, playback, or minimum-window layout.
- [ ] Chrome wide/narrow visual acceptance and reduced-motion behavior pass.
- [ ] A note saved against a stale base revision still commits, and the UI reports that another revision arrived rather than failing the save.
- [ ] A label removed from a stale client cache removes only server-observed add-tags and leaves a concurrent add intact.
- [ ] Session expiry during playback shows a re-authentication state, not a playback error.
- [ ] Seeking issues range requests rather than a whole-file fetch, verified from network records.

## Verification

Run component tests and Chrome Playwright flows against a real server with generated media. Include network interruption, session expiry, proxy range seeking, concurrent edits, offline Mac, and server restart. Run the desktop frontend build and manual regression pass.

## Handoff notes

Do not introduce rough-cut or review-status concepts as convenient UI state. Task 16 adds administration outside the focused review workspace.
