# Desktop app reference

See also [desktop-ui-conventions.md](desktop-ui-conventions.md) and [desktop/HARD_LESSONS.md](desktop/HARD_LESSONS.md).

## 7. Running the app

`pnpm dev` runs the real Tauri app (`RCDVLOG_CATALOG=/abs/catalog.sqlite3 pnpm dev` opens a catalog). `pnpm --dir apps/desktop dev --host 127.0.0.1` runs the Vite-only browser preview at `http://127.0.0.1:1420/` (`?clips=3000` seeds the scale fixture); it uses the CUJ harness as backend and can never validate native dialogs, Tauri-to-Rust calls, the media origin, FFmpeg, persistence, or filesystem permissions. The preview has no media origin, so no clip plays. Never treat a browser-only pass as an end-to-end media test. Builds: `pnpm --dir apps/desktop build`, `pnpm build:cli` (`target/debug/rcdvlog`), `pnpm build` (Tauri bundle), `pnpm build:server`, `pnpm build:web`. Server deployment: `docs/server/deployment.md`; security/pairing: `docs/server/security.md`. Never expose the raw `127.0.0.1:8090` backend or enable Funnel.

Rules that must not be violated:

- **Dev eval bridge** (`RCDVLOG_EVAL_BRIDGE=1 pnpm dev`, `pnpm eval -- '<js>'`) is a remote-code-execution surface. It must remain compiled out under `#[cfg(debug_assertions)]`, disabled unless the exact opt-in is present, bound to loopback only, and closed to every WebSocket handshake carrying an `Origin` header. Never turn it into a release diagnostic, runtime setting, or non-loopback listener. A bridge that cannot bind logs and the app starts anyway.
- **`RCDVLOG_MEDIA_DEBUG=1`** mounts `GET /debug/media` on the media origin (behind its token, `Host` and `Origin` guard); it reports full media paths and must stay compiled out of release. `apps/desktop/src-tauri/tests/release_dev_seams_absent.rs` proves both seams are absent.
- **Fixed ports are contracts; ephemeral ports are implementation.** A discovered listener binds `127.0.0.1:0`, reads the port back, and publishes it through a channel its consumer already reads (media origin, eval bridge). A config-pinned port gets a preflight naming the holder. Test fixtures allocate ports dynamically (`scripts/lib/loopback-port.mjs`). Never add a retry loop or fallback range.
- **Desktop 1420 and web 1421 are contracts**: `devUrl`, the Playwright base URL, and the media origin's allowed origins name 1420, and an origin mismatch fails silently. The `predev` preflight (`scripts/preflight-dev-port.mjs`) names the holder and offers a `kill` line only for a Vite inside this checkout; it never kills anything itself.

Eval-bridge env/address-file and media-debug workflow: [docs/dev-environment.md](dev-environment.md).

## 8. Project discovery, catalogs, and storage

New desktop projects default to one folder per project:

```text
~/Movies/VideoProjects/<safe project name>/
├── catalog.sqlite3
├── catalog.sqlite3-wal             # while active; SQLite-managed
├── catalog.sqlite3-shm             # while active; SQLite-managed
└── cache/
│   ├── proxies/<project-id>/<asset-id>.mp4
│   └── thumbnails/<asset-id>/q1.v2.jpg
```

The projects parent folder is configurable in global settings. The desktop scans immediate child directories for `catalog.sqlite3`; invalid catalogs are skipped. The settings file is stored in the macOS application-data directory for `com.rcd.vlogcutter` (normally under `~/Library/Application Support/`). A legacy application-support catalog is still used when the app/CLI is started without an explicit per-project catalog.

Catalog selection precedence for the CLI:

1. `--catalog /absolute/path/catalog.sqlite3`
2. `RCDVLOG_CATALOG`
3. The desktop's persisted `lastCatalogPath`
4. Legacy application-support catalog

Always pass `--catalog` in agent automation, tests, and temporary workflows. A desktop project folder can otherwise be confused with the legacy catalog.

SQLite connections enable foreign keys, WAL journal mode, and a five-second busy timeout. Read operations can continue during an uncommitted writer. Bundled SQLite must stay at 3.51.3 or newer (`rusqlite` 0.39+): 3.51.1 deadlocks when two connections to one WAL database close at the same time, and the catalog opens a connection per operation (guarded by `core/rcdvlog-core/tests/sqlite_concurrent_close.rs`, #258). Project mutations append typed operations with materialized revisions. Adopted projects synchronize bounded operation pages between independent local catalogs; live SQLite/WAL files are never copied or placed on NAS.

Project catalogs use an explicit `schema_migrations` history and are currently at version 66. The server control catalog is version 17; backup format is version 1. Migration definitions have immutable names/source identities and each migration commits atomically. Migration 18 includes a one-time compatibility reconciliation for catalogs created on the import feature branch before its migration IDs were merged with main. When changing the schema:

- Preserve existing catalogs; never solve migration issues by deleting the database.
- Add a forward migration and an upgrade test using an older schema fixture.
- Back up or otherwise provide recovery before any destructive rewrite.
- Keep Rust row order, Serde DTOs, TypeScript mirrors, CLI JSON tests, and queries synchronized.
- Remember that `Catalog::open_existing` validates that `projects` and `assets` exist before opening read/write and migrating.

## 10. Desktop UI conventions

Keyboard-first and dense; domain logic stays out of `App.tsx`. Full API/state, ribbon, CSS, and top-bar-ladder detail: [docs/desktop-ui-conventions.md](desktop-ui-conventions.md).

- `api.ts` selects real Tauri calls when `__TAURI_INTERNALS__` exists, else the harness backend. Optimistic asset updates replace the returned canonical `Asset`; failures roll back and surface an error. Merge completed assets from `proxy-progress`, `thumbnail-progress`, and `analysis-progress` events immediately.
- Review video is **not** served over the Tauri asset protocol (`WKWebView` yields no first frame through a custom scheme). The app binds its own loopback HTTP origin (`media_server.rs`, docs/tasks/desktop/MEDIA_DELIVERY.md (`4c5eb2f:docs/tasks/desktop/MEDIA_DELIVERY.md`), [docs/desktop/HARD_LESSONS.md](desktop/HARD_LESSONS.md)). Its eight security requirements are acceptance criteria with tests; never weaken one or widen the bind: loopback bind, session token, asset ids rather than paths, storage-root re-validation, `Host`/`Origin` checks, `GET`/`HEAD` only, no token or path in logs, stop with the app. `api.mediaUrl` carries the proxy-versus-original choice (`rep=`); on a media `error`, `api.recoverMediaOrigin()` restarts the origin at most once per session.
- Playback controls are custom stage-level UI; do not re-enable native `<video controls>` (they inherit the rotation transform). Playback keeps a bounded three-slot window (previous/current/next); do not widen it without measurement. Autoplay is `never`, `proxyAvailable` (default), or `always`.
- Thumbnails and posters still use the asset protocol (scope: app-data cache, `$HOME/**`, `/Volumes/**`, temp fixtures). If media paths outside it are introduced, update that scope and the loopback origin's storage roots deliberately; never disable security globally.
- Layout: minimum window 980x680, default 1440x900. Keep visible focus, semantic labels, `aria-keyshortcuts`, and the dark editing-bench look. The top bar degrades rather than clips: anything added to it must be bounded in width, and the bar clips across but not down (`overflow-x: clip`).

### Keyboard contract

Preserve and update both visible hints and the `?` keyboard map when shortcuts change.

- `J`/`K` or arrows: next/previous clip. Space: play/pause. `L`: label field (Enter adds). `R`: remove/restore. `N`: focus notes (consume the `n` keydown first). `/`: queue search.
- Notes: Enter saves; Command+Enter newline; Shift+Enter saves and advances; Escape discards the editing session and blurs. After advancing, notes must not remain focused.
- `P` selected proxy; Shift+`P` missing proxies; `T` missing thumbnails; `A` analyze project; `M` add media; `[`/`]` ribbon zoom; `C` center clip; `O` rotate 90 degrees clockwise; Shift+`O` flip 180; `F` Focus; Cmd+`1`/`2`/`3` modes; Shift+`T` Chronology desk.
- Command+`N` new project; Command+`O` open catalog; Command+`,` global settings; `?` keyboard map.

Global shortcuts return early for text inputs, textareas, and selects (except modal Escape). A shortcut that moves focus must call `preventDefault()` first.
