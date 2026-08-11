# Integration testing

The repository uses real process, SQLite, HTTP, browser, and media-tool boundaries where automation can represent production behavior. Mocks are limited to fast UI development and are never release evidence for native or network behavior.

## Automated coverage

| Boundary | Checked behavior |
| --- | --- |
| Rust workspace | Migrations, catalog/domain operations, sync/conflicts, storage identity, scans, jobs, representations, auth/security, backup/restore, health, and diagnostics |
| Desktop CLI processes | Stable JSON stdout/stderr, explicit catalogs, WAL reads, real FFmpeg ingest/metadata/dedup, labels/notes/removal/clock, thumbnails/proxies, and offline-original review |
| Card-import domain/UI | Preflight purity and classification, removable-card identity, create-only atomic copy, full-file verification, cancellation/resume, archive-root remount, modal destination/capacity blocking, and review-only card banner |
| Independent server processes | Separate Mac/server SQLite files, real authenticated HTTP baseline/reconcile, duplicate delivery, browser-origin mutation, restart, and restore sequence recovery |
| Fast desktop CUJ | Whole-`App` journeys in jsdom against a fake Tauri IPC destination: review and annotation, search/filtering, generation accounting, analysis gating/progress/cancellation, chronology preview-before-write, edit-plan lifecycle, card import, failure and retry, and IPC command/argument contract |
| Desktop real-browser | Headless Chromium over the Vite preview: shell layout at 1280×800 and 1440×900, no region taking an implicit grid row, no body scroll, Focus reflow with real DOM focus and caret, selected-row scroll-into-view, splitter pointer-drag/reset/keyboard, and queue accessibility-tree and keyboard navigation |
| Web unit/build | Typed client integration, loading/error/empty states, library/project review, annotations, and administration |
| Chrome Playwright | Local-only bootstrap login, admin mutation, concurrent same-origin tabs, and durable 5-second polling when one WebSocket is disabled |
| Scale/fault fixtures | 2,000-asset baseline, 2,000-row library/scan pages, 10,000 jobs, sequence gaps/lost acknowledgements, leases/retry/cancel, media cancellation/restart, and byte ranges |
| Static contracts | OpenAPI drift, loopback/Compose/read-only-original/Funnel deployment invariants, frozen protocol schemas, and Markdown links |

Representative gates:

```bash
PATH="/opt/homebrew/bin:/usr/local/bin:$PATH" cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
pnpm check:frontend
pnpm test:web
pnpm test:web:e2e
pnpm test:desktop:browser
pnpm check:openapi
pnpm check:deployment
pnpm check:docs
```

`pnpm test:integration` runs the desktop CLI/media process suite. FFmpeg and ffprobe are required for its media case; absence skips media rather than pretending a mock passed. `pnpm check` combines frontend type checking with Rust workspace tests.

## What the fast desktop CUJ suite proves

`apps/desktop/src/test/cuj` boots the production `App` in jsdom and installs a
deterministic in-memory backend at `window.__TAURI_INTERNALS__`, so the real
`api.ts`, `chronologyApi.ts`, command names, argument shapes, and event
subscriptions are all under test while only the destination is faked. Its
contract is `docs/tasks/harness/ARCHITECTURE.md`; the production findings it has
produced so far are recorded in `docs/tasks/harness/FINDINGS.md`.

It does validate: that a reported write reached the backend and is visible to
the next read, that reported generation counts equal the assets actually
changed, that a cancelled analysis cannot present itself as a completed one,
that chronology previews before it writes and captures the user's place before
mutating, that an import adds exactly the clips its preflight classified as new,
and that every command the frontend can issue is handled and registered.

It does not validate — and may never be cited for — WKWebView behavior, video
decoding or first-frame paint, loopback media transport, filesystem permissions,
native dialogs, Keychain, sleep/wake, NAS behavior, FFmpeg, the chronology
solver, edit-plan generation, OTIO contents, or SQLite persistence and
migrations. Its media is logical playback state driven by DOM events, its
chronology and validation results are scenario-declared outcomes, and its import
copies no bytes.

## What browser modes prove

The desktop Vite preview at port 1420 runs the same in-memory fake backend as the CUJ harness: `main.tsx` installs it from `apps/desktop/src/preview/` when the app boots outside Tauri, so there is one fake to keep honest rather than two. It is appropriate for layout, focus, keyboard, scrolling, and for watching progress and cancellation behave. It cannot validate Tauri commands reaching Rust, Keychain, native media playback, filesystem permission prompts, persistence, media processes, or sync — and it has no media origin, so no clip plays.

The desktop real-browser tier (`pnpm test:desktop:browser`, `apps/desktop/e2e/`) drives that preview in headless Chromium through Playwright. Every scenario proves a property a jsdom CUJ cannot: a resolved grid track, a real scroll position, genuine DOM focus and caret, real pointer-capture drags, and Chromium's own accessibility tree — which is why it is deliberately under thirty scenarios and never re-runs the CUJ matrix. It shares the fast tier's boundary exactly: a real engine is still not WKWebView, so it is never evidence for native media, IPC, Keychain, dialogs, or persistence. It found the Focus-mode workbench keeping its Review column widths (an inline style outranking the stylesheet), which was fixed rather than pinned because it is a live layout defect.

The web Playwright suite uses a real disposable server and Chrome. It does validate browser authentication, API mutation, multi-tab convergence, WebSocket loss, and polling recovery on one host. It does not validate Caddy certificates, Tailscale ACLs/routing, physical LAN latency, or NAS behavior.

## Manual and target-topology acceptance

These checks require the actual Mac/Ubuntu/NAS/LAN/Tailscale topology and remain unaccepted until recorded:

- Render/start Compose on Ubuntu, prove the data volume is local SSD, and exercise reboot/restart policy.
- Mount NFS (preferred) or SMB originals read-only, verify a differently named Mac binding, scan exact counts, and prove the original tree/metadata unchanged.
- Install/trust the Caddy internal CA, then pair/adopt/review/seek in Chrome and the native Tauri app over LAN HTTPS.
- Repeat endpoint failover, sync, streaming, revocation, and offline review through Tailscale Serve with Funnel off.
- Disconnect/reconnect NAS during scan/hash/proxy; queue Mac-only work; download a proxy and review with NAS unavailable.
- Record server CPU, RSS, throughput, temperature, and throttling on the target laptop under representative media load.
- Repeat backup/restore in Compose and reconnect a retained Mac history.
- Exercise macOS dialogs, filesystem permissions, native media playback, sleep/wake, and clean-machine packaging.
- Insert one real camera card twice with additional footage between insertions;
  prove only the added clips copy on the second pass. Relocate the resulting
  archive between two physical volumes, rebind it once, and prove every asset
  resolves while the card and both archive trees remain byte/metadata
  unchanged except for newly created destination files.
- Run periodic thousands-of-assets/roughly-100-hour NAS acceptance. The real Expo source fixture is never an automated destructive target.

The pure-Rust OTIO writer is implemented and covered by deterministic unit
tests. DaVinci Resolve 20/21 import remains a manual desktop MVP release gate;
headless tests do not establish editor compatibility.

The authoritative last-run matrix, including every “not run” reason and residual risk, is [Task 19 acceptance](tasks/server/ACCEPTANCE_TASK_19.md). Passing local automation is not permission to relabel those external checks as passed.
