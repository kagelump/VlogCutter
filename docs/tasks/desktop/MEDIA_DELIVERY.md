# Media delivery — replace the asset protocol with a loopback HTTP origin

**Status** proposed. **Depends on** nothing. **Supersedes** the current
`convertFileSrc` path in `apps/desktop/src/api.ts`.

## The problem

The review stage cannot show a still frame of a clip that has not been played.
`WKWebView` does not produce video frames for media delivered through a
`WKURLSchemeHandler` custom scheme, which is what Tauri's `asset://` is on
macOS. The element reports `readyState 4`, correct dimensions and no error, and
holds nothing paintable. Full evidence, including the two-video A/B that
isolates the scheme as the sole variable, is in
`docs/desktop/HARD_LESSONS.md`.

Playback is unaffected — `play()` engages the pipeline — so this reads as "some
clips have previews and some don't", where the ones with previews are exactly
the ones the editor happened to play.

This is not a bug we can fix where it lives, detect reliably at runtime, or
work around in CSS. It is a property of the transport, and it will be inherited
by every future media surface: hover scrub, filmstrips, contact sheets,
side-by-side compare.

## Why not the cheaper options

**Generate thumbnails and rely on `poster`.** Worth doing regardless — it is
the mechanism `VideoStage` was designed around, images are unaffected by the
limitation, and `Shift+G` fixes today's symptom on an existing project with no
code change. It does not fix un-played video generally: the poster disappears
the moment playback is requested, seeking to an unplayed region has the same
problem, and any surface that wants a frame without a poster still gets
nothing.

**`dangerousUseHttpScheme`.** Switches the app's own origin to `http://` on
macOS but keeps serving it through a custom scheme handler. Same mechanism,
same defect, plus a weaker origin.

**Detect and remount.** Tested against the live app: a no-op seek does not
recover the element, releasing the other mounted elements does not, and a
freshly created element as the sole occupant does not either. There is nothing
to fall back *to* while the transport stays the same.

## Proposal

Serve review media from a loopback HTTP origin owned by the desktop app.

- On app start, bind a server to `127.0.0.1` on an **ephemeral port** (`:0`,
  then read back the assigned port). Never `0.0.0.0`.
- Mint a **per-session bearer token** (≥128 bits from a CSPRNG) at startup.
- Expose the base URL and token to the frontend through an existing Tauri
  command; they never touch disk and never outlive the process.
- `api.mediaUrl(asset)` returns `http://127.0.0.1:<port>/media/<assetId>?t=<token>`
  instead of `convertFileSrc(path)`. Resolution of proxy-vs-original stays
  exactly as it is today.
- The handler resolves `assetId` through the catalog, so **no filesystem path
  is ever accepted from the client**.
- Full `Range` support, streaming from disk. No whole-file reads: the current
  asset protocol reads an entire file into memory for a non-range `GET` and
  caps ranges at 1000 KiB, which is ~14 round trips per second of 4K footage.

`core/rcdvlog-server` already carries an HTTP stack and range-request handling
worth borrowing rather than reimplementing.

## Security requirements

A loopback server is reachable by **every process on the machine** and, for
some request shapes, by any web page the user has open. These are not optional.

1. **Bind `127.0.0.1` only.** Never `0.0.0.0`, never a LAN interface.
2. **Require the session token** on every request. Reject without it before any
   catalog or filesystem access.
3. **Accept asset ids, not paths.** Resolve through the catalog. There is no
   endpoint that takes a path, and therefore no traversal surface.
4. **Re-validate the resolved path** against the project's storage roots before
   opening it, so a corrupted or hostile catalog row cannot escape either.
5. **Reject unexpected `Origin` and `Host` headers.** This is the DNS-rebinding
   defence: a page on the public internet can resolve a hostname to `127.0.0.1`
   and issue requests. Allow only the app's own origin; refuse anything else.
6. **`GET` and `HEAD` only.** No writes, no directory listing, no ranges over
   anything but registered assets.
7. **Do not log tokens** or full media paths at info level.
8. **Stop the server with the app**, and drop the token with it.

## Acceptance criteria

- A clip that has never been played shows its first frame in the stage, on a
  project with no proxies and no thumbnails.
- `drawImage` on the active element returns non-zero alpha before any `play()`.
- Seeking to an unbuffered region of an unplayed clip renders that frame.
- Neighbour preloading (`VideoStage` prev/next) continues to work.
- A request without the token gets 403 before any disk access.
- A request with a foreign `Origin` gets 403.
- An asset id outside the open project gets 404.
- Server is unreachable after the app exits.
- Existing desktop tests stay green; `api.mediaUrl` keeps returning `null`
  outside Tauri so the mock path is unchanged.

## Risks and rollback

| Risk | Mitigation |
| --- | --- |
| Port binding fails or conflicts | Ephemeral port; surface a clear startup error rather than failing silently at first playback |
| Server dies mid-session | Health check on media error; restart once and report |
| Security requirements implemented partially | They are acceptance criteria above, not prose; test each |
| Regression in playback | `mediaUrl` is a single function — reverting to `convertFileSrc` restores today's behaviour exactly |

Rollback is one function body, which is the main argument for doing this behind
`api.mediaUrl` rather than threading URLs through the component tree.

## Out of scope

Streaming transcode, HLS, remote/companion-server media, and any change to
proxy or thumbnail generation. This task changes **how bytes reach the webview**
and nothing else.
