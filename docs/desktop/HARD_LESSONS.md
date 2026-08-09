# Hard-learned lessons — desktop

Findings that cost more to discover than they take to read. Each entry records
the symptom as it presented, the evidence that settled it, and — where it
matters — the plausible explanations that turned out to be wrong, because the
wrong turns are usually the expensive part.

Append new entries at the top. Keep the evidence, not just the conclusion: a
lesson without its measurement gets re-litigated in six months.

---

## WKWebView does not render video delivered over a custom URL scheme

**Discovered** 2026-08-09, investigating "previews are not showing up for some
clips" on the 2026 Eclipse project (85 clips, no proxies, no thumbnails).

### The symptom

A clip selected in the review stage shows a black frame. The element is not
broken in any way it will admit to:

| Property | Value |
| --- | --- |
| `readyState` | `4` (HAVE_ENOUGH_DATA) |
| `networkState` | `1` (NETWORK_IDLE) |
| `buffered.end(0)` | `16.72` of a 17.47s clip |
| `videoWidth` / `videoHeight` | `3840` / `2160` |
| `error` | `null` |
| `drawImage` → `getImageData` | `[0,0,0,0]` — **alpha 0** |

That last row is the tell. The canvas readback is not a dark frame, it is
*nothing at all*: the element holds no presentable frame despite reporting that
it holds enough data to play through. Playback still works — calling `play()`
engages the pipeline and frames flow from then on, permanently. So the app is
usable, and the only broken thing is the still frame of a clip you have not yet
played, which is exactly what a "preview" is.

### The cause

Tauri delivers local media as `asset://localhost/<path>` (`convertFileSrc`,
see `apps/desktop/src/api.ts` `mediaUrl`). On macOS that is a
`WKURLSchemeHandler` custom scheme. **WKWebView parses such a resource, reports
full readiness and correct dimensions, and never produces a video frame for
compositing.**

Delivery mechanism is the entire variable. Same file, same `WKWebView`, same
configuration, same instant, two `<video>` elements side by side:

| Source | `readyState` | `videoWidth` | On screen |
| --- | --- | --- | --- |
| `asset://` (Tauri scheme) | 4 | 3840 | **black** |
| `http://127.0.0.1` (identical bytes) | 4 | 3840 | **paints** |

### Reproducing it

A standalone Swift harness settles this in about a minute and does not need the
app. Build a `WKWebView`, register a `WKURLSchemeHandler` for `asset` that
serves a file, put two `<video preload="auto" muted>` elements on the page —
one pointed at the custom scheme, one at a plain HTTP server serving the same
file — wait for both to reach `readyState 4`, then `takeSnapshot` and look.

Two details that will otherwise waste an hour:

- **Read the result with a snapshot, not a canvas.** Mixing a cross-origin
  HTTP video into a custom-scheme page taints the canvas, and `getImageData`
  throws `SecurityError`. Screenshot the window instead.
- **Replicating the protocol's *semantics* over HTTP proves nothing.** An HTTP
  server that mimics `asset.rs` byte-for-byte — the 1000 KiB range cap, the
  `Accept-Ranges` header behaviour — plays perfectly. The custom scheme is the
  variable, not the response shape.

### What follows from it

- Any un-played clip is a black rectangle unless something else is drawn there.
  The stage's `poster` is that something: `VideoStage` sets it from the clip's
  thumbnail. **A project with no generated thumbnails has no posters and
  therefore no previews at all** — `Shift+G` (`makeMissingThumbnails`) is the
  cheap remedy, and images are unaffected by this limitation.
- The real repair is to stop using the custom scheme for media. See
  `docs/tasks/desktop/MEDIA_DELIVERY.md`.
- Assume this affects every future media surface — hover scrub, filmstrips,
  multi-clip contact sheets — not just the stage.

### What was done about it

Video now comes from a loopback HTTP origin the app binds at startup
(`apps/desktop/src-tauri/src/media_server.rs`); `api.mediaUrl` addresses it by
asset id with a per-session token. Thumbnails and posters still use
`convertFileSrc`, because the limitation is video-only.

The row that matters in the A/B above — `http://127.0.0.1`, identical bytes,
paints — is the whole reason for the change, and it has **not yet been
re-confirmed in the app itself**. Until someone opens a project with no
thumbnails and sees a first frame, treat the fix as reasoned rather than
observed.

### What this was not

Six explanations were tested and falsified before the real one. They are
recorded because each looked convincing, and because re-testing them is waste.

| Hypothesis | How it died |
| --- | --- |
| Something differs between the working and failing clip | Catalog rows identical in every relevant column; both HEVC Main 10, 3840×2160, 59.94fps, `moov` at end |
| The external drive is too slow to feed 4K | `dd` measured 365 MB/s on the Acasis volume |
| Tauri's 1000 KiB range cap starves the decoder | Reimplemented `asset.rs` semantics over HTTP; `readyState 4` in ~0.7s in both Chromium and WKWebView |
| Large files cross some buffering threshold | 151 MB and 241 MB clips behaved identically over HTTP |
| Three concurrent 4K HEVC decoders exhaust WebKit | Reduced the stage to a single mounted element; still black. A *fresh* element, sole occupant, also black |
| The 1×1 `.preloaded` CSS breaks the layer on promotion | Replicated the exact CSS; the promoted element painted. In the live app the 1×1 neighbours held *good* frames while the full-size active one held none |

The neighbour-buffer change made on the fifth hypothesis was reverted. The
`preload`/1×1 neighbour design in `VideoStage` is not implicated.

---

## Working-practice lessons from the same investigation

**Confirm the mapping from what the user sees to what you query.** The report
was "#21 has a preview and #22 does not". Ribbon position was assumed to equal
row number in `best_time` order and never checked. It happened to be correct,
but the assumption went unverified for most of the investigation and was at one
point wrongly blamed for a failed reproduction — burning a cycle in both
directions. One console line resolves it: read `src` and `data-asset-id` off the
live element.

**Reproduce the mechanism, not the contract.** The single most expensive error
was building a faithful HTTP simulation of the asset protocol's responses. It
was correct in every observable detail and reproduced nothing, because the
defect lived in the transport the simulation replaced. When a bug resists a
faithful-looking repro, suspect the thing the repro abstracted away.

**`drawImage` + `getImageData` is the probe for "decoded but not presentable".**
`readyState`, `networkState`, `buffered` and `error` all report healthy in this
failure. Alpha 0 from a canvas readback is the only signal that separates "has
no frame" from "has a frame you cannot see". Reach for it early on any
black-video report.

**Never `git checkout -- <file>` to undo your own edit in a dirty tree.** Doing
so during this investigation destroyed uncommitted work in `App.tsx` — a
complete camera-rename wiring and a judgment-rail fix. Reverse your own edit
with a targeted edit, or `git stash push -- <file>` first. Recovery here was
only partial and only possible because a typecheck error named the missing prop
and an untracked test file specified the lost behaviour; neither is something
to count on. Commit or stash before touching a working tree you did not create.
