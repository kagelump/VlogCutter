# Chronology desk: make the footage first class

Date: 2026-08-12

## Problem

The chronology desk is a full-screen takeover (`App.tsx` renders `CalibrationPanel`
instead of the workbench when `view.mode === "chronology"`). It contains no video and
no thumbnails. A user correcting clip order therefore judges "is this clip in the
right place?" from a filename and a timestamp alone.

Two concrete failures:

1. **No footage.** The one thing that settles an ordering question — looking at the
   frame — is unavailable without leaving the desk.
2. **Truncated tiles.** `.chronology-block` is `calc(var(--chronology-column) - 10px)`
   wide, i.e. 58px at the default 68px zoom. Into that go a filename, a counter, and
   `formatChronologyTime`, which emits strings like `Feb 8, 14:02:31 · Asia/Tokyo`.
   Both the name and the time ellipsise, so the tile carries almost no information.

## Design

### 1. Shared media buffer

`App.tsx` derives `bufferedMedia` inline: prev/active/next asset, proxy-versus-original
choice, rotation resolved against the representation, poster, display aspect. The desk
needs the identical derivation over a different order (solved order, not queue order).

Extract to `src/review/mediaBuffer.ts`:

```ts
buildMediaBuffer(ordered: Asset[], activeId: string | null, origin: MediaOrigin | null): BufferedMedia[]
```

Pure and unit-testable. `App.tsx` and `CompareDeck` both call it. The rotation and
representation rules exist once.

`BufferedMedia` moves with it; `VideoStage` keeps importing the type.

### 2. Compare deck

`src/chronology/CompareDeck.tsx`, a new row in `.chronology-canvas-panel` between the
heading and the canvas scroller.

- **Centre**: the focused clip as a live `<video>` served by the existing loopback
  media origin, with poster, rotation, and quarter-turn handling matching `VideoStage`.
  A slim transport: play/pause, scrub, mute, elapsed/duration.
- **Flanks**: the clips immediately before and after **in solved order**
  (`overview.assets`), as large thumbnails captioned with filename and placed time.
  Clicking one focuses it.
- Neighbours are mounted as preloaded `<video>` elements, matching Review's ±1 buffer,
  so stepping is instant.
- Collapsible. Collapsed state lives in `CalibrationPanel`.

Solved-order neighbours rather than same-lane neighbours: the desk's question is about
global ordering across cards, and a wrong interleave is exactly what the eye catches.

Degradation: no media origin (browser preview) or no proxy yields the same offline
slate `VideoStage` uses, stating why.

### 3. Thumbnail tiles

`.chronology-block` becomes a thumbnail tile:

- Thumbnail fills the tile, `object-fit: cover`, rotated via `renderedRotation(asset,
  isRawOrientationThumbnail(asset))` — the pair `QueuePanel` already uses.
- A persistent ordinal badge in the corner.
- The placed time as an overlay revealed on `:hover` and `:focus-visible`.
- Filename and full time remain in `aria-label` and gain a `title`, so assistive tech
  and the existing CUJ assertions are unaffected.
- No thumbnail on disk: the current text tile renders as fallback.

Tile and lane height derive from column width through a CSS custom property so tiles
stay roughly 16:9 across the 44–116px zoom range, with a floor so the smallest zoom
still shows a legible frame.

A zoom control moves into the desk toolbar, driving the same `chronologyZoom` state the
Review ribbon uses, so the viewport carry-over between the two surfaces is preserved.

### 4. Time ruler

The `Solved order` strip prints ordinals today. Ordinals move onto the tile badges and
the strip becomes a time ruler.

New pure function in `chronology/geometry.ts`:

```ts
chronologyTimeTicks(assets, columnWidth, window, minSpacingPx): TimeTick[]
```

A tick roughly every `minSpacingPx` (140 by default), carrying the ordinal it sits at
and a label. The label is the date when the calendar day differs from the previous
tick, and the time otherwise. Wall-clock fields are read from `displayTime.primary`
without `Date` round-tripping, for the reason `formatChronologyTime` documents: Rust
owns timezone resolution and reparsing would relabel a Tokyo clip in the laptop's zone.

## Testing

- `src/review/mediaBuffer.test.ts` — ordering, missing proxy, rotation against
  representation, absent origin.
- `src/chronology/geometry.test.ts` — tick spacing, day rollover, empty and
  single-asset lists, windows past the end.
- `src/chronology/ChronologyCanvas.test.tsx` — thumbnail rendered, text fallback,
  hover time overlay, ordinal badge, existing selection behaviour unchanged.
- `src/chronology/CompareDeck.test.tsx` — neighbours drawn from solved order,
  click-to-focus, offline slate.
- `src/test/cuj/chronology.cuj.test.tsx` — the deck appears and steps.

CI is quota-blocked, so verification is local: `vitest run`, `tsc -b`, `cargo test`,
and finally the real Tauri app opened against a copy of the 2026 Eclipse project.
