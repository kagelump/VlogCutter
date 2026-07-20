# Task 13: Web Foundation, Typed Client, and Shared UI

**Status:** Planned
**Depends on:** Task 9
**Unblocks:** Tasks 14, 15, and 16

## Objective

Create the Chrome-targeted React web application, generated/checked API client, shared `ProjectClient` seam, reusable review-component package, and automated frontend test foundation.

## User outcome

Chrome can authenticate to the server and load a responsive, accessible application shell that is ready to host library, project, review, and administration features.

## Scope

- Add `apps/web` Vite/React/TypeScript workspace.
- Expand `pnpm-workspace.yaml` for shared packages.
- Add `packages/project-client` with transport-neutral interfaces and shared DTO projection.
- Generate or verify the HTTP TypeScript client from the Rust-owned OpenAPI contract.
- Adapt the desktop API boundary to a `TauriProjectClient` without changing user behavior.
- Add `HttpProjectClient` with session/CSRF/error handling.
- Extract only the review primitives genuinely consumed by both surfaces into `packages/review-ui`.
- Add web routing/shell, session bootstrap/login, error boundary, loading/empty/offline states, and global server status.
- Add React unit/component testing and Chrome Playwright infrastructure.
- Serve built web assets from the Rust server under the same origin.

## Out of scope

- Library/project feature pages
- Proxy playback
- Annotation editing
- Administrative controls
- Pixel-identical desktop/web layouts
- Broad rewrite of the current desktop `App.tsx`

## Design requirements

1. React components call a client interface; they do not call `fetch`, Tauri `invoke`, or storage APIs directly.
2. The generated HTTP schema/client is checked for drift in CI.
3. The desktop continues to route native calls through its existing exported `api` boundary, wrapped/adapted rather than bypassed.
4. The web shell is distinct and responsive; only lower-level review components are shared.
5. Authentication uses Task 9 session and CSRF contracts.
6. Error handling preserves stable server error codes and request IDs.
7. The web build is deterministic and packaged with the server image/binary without requiring a Node runtime in production.
8. Chrome is the support target. Use standard H.264/MP4 and browser APIs supported by current Chrome.
9. Visible focus, semantic landmarks, labels, keyboard navigation, reduced-motion behavior, and contrast are required.
10. Desktop keyboard/focus contracts must not regress during component extraction.
11. Frontend tests use real DTO fixtures and mock the client interface, not internal `fetch` details.
12. Browser tests run against the actual server HTTP contract where integration matters.

## Likely implementation areas

- `pnpm-workspace.yaml`
- `apps/web/`
- `packages/project-client/`
- `packages/review-ui/`
- `apps/desktop/src/api.ts`, `types.ts`, and selected components
- `apps/server` static-asset fallback and cache policy
- OpenAPI schema/generation scripts
- Root package scripts and lockfile

## Acceptance criteria

- [ ] `pnpm --dir apps/web build` produces static assets served by the server.
- [ ] Unauthenticated Chrome receives the bootstrap/session flow rather than product data.
- [ ] Authenticated Chrome loads the application shell and global server status.
- [ ] Session expiry, credential revocation, CSRF failure, server unavailable, and protocol mismatch have distinct usable states.
- [ ] `HttpProjectClient` and `TauriProjectClient` satisfy the same compile-time interface for shared operations.
- [ ] API schema drift fails a checked command.
- [ ] Shared component extraction does not introduce direct Tauri or HTTP calls.
- [ ] Existing desktop TypeScript build and interaction behavior pass.
- [ ] Web component tests and a Chrome Playwright login/shell smoke test run in automation.
- [ ] Production server image contains no development server and needs no Node runtime.
- [ ] Web layout is usable at desktop Chrome widths and a narrower tablet-like viewport, with phone treated as status/browse best effort.

## Verification

Run desktop and web TypeScript checks/builds, component tests, Playwright smoke tests, and a production-static-assets server test. Visually inspect the shell in Chrome at representative wide and narrow sizes.

## Handoff notes

Freeze the client and shell extension points before feature tasks. Avoid speculative packages for timeline, trip context, scripts, or edit planning.
