# Desktop UI conventions

Relocated verbatim from the repository-root `AGENTS.md` section 10 ("Desktop UI conventions") so the root file can stay a short index (issue #260). The keyboard contract is in [desktop.md](desktop.md).

The UI is intentionally keyboard-first and dense, using one main `App.tsx` today. Keep domain logic out of it even if component extraction is useful.

## API and state

- `apps/desktop/src/api.ts` selects real Tauri calls when `__TAURI_INTERNALS__` exists and mock behavior otherwise.
- Optimistic asset updates call the core and replace the returned canonical `Asset`; failures roll back and surface an error.
- Progress events are `proxy-progress`, `thumbnail-progress`, `analysis-progress`, and `model-install-progress`. Each of the first three carries an optional completed asset plus a batch estimate. Merge a completed asset immediately so thumbnails, proxies, and generated titles appear as they finish. `analysis-progress` additionally carries the current stage and the run's spend so far.
- Media playback prefers an available proxy, otherwise an available original. Thumbnails are also used as the video's poster frame.
- `api.mediaUrl` builds a URL against the loopback media origin, passing the asset id and the representation it chose (`rep=proxy` or `rep=original`) plus the session token. The base URL and token arrive on `mediaOrigin`, returned by `desktopSettings` and the other settings commands and held in memory only. Keep the proxy-versus-original decision here, next to rotation and poster agreement, rather than moving it into the origin.
- When a mounted element fires `error`, the shell calls `api.recoverMediaOrigin()`, which health-checks the origin and restarts it once per session. A healthy origin is returned unchanged so an ordinary decode failure does not invalidate every URL on screen.
- Playback controls are custom stage-level UI so they remain horizontal while the media element rotates. Do not re-enable native `<video controls>` unless rotation is moved off the video element; native controls inherit its CSS transform.
- The React `assets` array is the in-memory metadata cache. Legacy orientation probing uses a two-worker, deduplicated priority queue for current, next, next-next, and previous clips; successful probes persist to SQLite and replace the cached asset.
- Playback uses a bounded three-slot sliding window (previous/current/next). The next available representation is preloaded automatically (proxy preferred, otherwise one original), the previous slot stays mounted but requests metadata only, and clips outside the window are unmounted. Preserve these bounds unless memory, file-handle, and NAS-read measurements justify a wider cache.
- Global autoplay settings are `never`, `proxyAvailable` (default), and `always`.

## Keyboard contract

Preserve and update both visible hints and the `?` keyboard map when shortcuts change. Current important behavior:

- `J`/`K` or arrow keys: next/previous clip.
- Space: play/pause.
- `L`: focus the free-form label field; Enter adds the label.
- `R`: remove the selected clip from the labeling queue, or restore it in the Removed view.
- `N`: focus notes. Consume the original `n` key event before focusing so it is not inserted.
- Notes: Enter saves; Command+Enter inserts a newline; Shift+Enter saves and advances; Escape discards the current editing session and removes focus. After advancing, notes must not remain focused.
- `/`: focus queue search.
- `P`: selected proxy; Shift+`P`: missing proxies; `T`: missing thumbnails; `A`: analyze project.
- `M`: add media.
- `[`/`]`: story-ribbon zoom; `C`: center selected clip.
- `O`: rotate the selected clip 90° clockwise; Shift+`O`: flip it 180°.
- Command+`N`: new project; Command+`O`: open catalog; Command+`,`: global settings; `?`: keyboard map.

Global shortcuts must return early for text inputs, textareas, and selects, except modal-level Escape handling. Any shortcut that moves focus must call `preventDefault()` before focusing the destination.

## Story-ribbon context

The ribbon already accepts optional TypeScript fields `phase`, `location`, `timezone`, and `storyBeat` and groups contiguous equal contexts. These fields are populated only in the browser demo. When persisted context is added, prefer a core-derived per-asset display projection from explicit time/range records rather than making the optional UI fields an independent source of truth.

## CSS and layout

The app has a minimum 980×680 window and a three-column workbench. Review queue and inspector scroll independently; the player and story ribbon occupy the center. Preserve visible focus styles, semantic labels, `aria-keyshortcuts`, and reduced-clutter dark editing-bench aesthetic. Test at the configured 1440×900 default and the 980×680 minimum.

The top bar is one row that degrades rather than clips (#75). Its ladder is in
`styles.css` under "The degradation ladder": stage 1 drops the wordmark word,
the full progress form, and the lifecycle labels (into `•••`); stage 2 drops
the mode switcher's words to glyphs and the `?` button into the same menu. The
`N/N` count, Stop/Cancel, Add media, and the project switcher never give way
and are asserted whole and clickable in `e2e/topbar-degradation.spec.ts`. Each
stage is a container query on the bar with a wider twin for
`[data-operation="active"]`, because a running operation claims ~400px of the
same row. Two rules keep it working: anything added to the bar must be bounded
in width — the operation slot's compact form relocates its variable text to a
tooltip for exactly that reason — and the bar clips across but not down
(`overflow-x: clip`), because its own dropdown menus open below it.

Review video is **not** served over the Tauri asset protocol. `WKWebView` never
produces a video frame for media delivered through a custom scheme, so an
un-played clip has no first frame; the app binds its own loopback HTTP origin at
startup and `api.mediaUrl` points at that instead. See
`apps/desktop/src-tauri/src/media_server.rs`, `4c5eb2f:docs/tasks/desktop/MEDIA_DELIVERY.md`,
and the evidence in `docs/desktop/HARD_LESSONS.md`. Its eight security
requirements — loopback bind, session token, asset ids rather than paths,
storage-root re-validation, `Host`/`Origin` checks, `GET`/`HEAD` only, no token
or path in logs, stop with the app — are acceptance criteria with tests, not
prose. Do not weaken one to make a surface easier to build, and never widen the
bind address.

Thumbnails and posters still use the asset protocol, which permits app-data
cache paths, `$HOME/**`, `/Volumes/**`, and temporary RCD VlogCutter cache
fixtures. If media paths outside these scopes are introduced, update both that
scope and the loopback origin's storage roots deliberately, validate on macOS,
and do not disable security globally as a shortcut.
