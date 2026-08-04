# Desktop review layout overhaul

Status: design approved 2026-08-04. Supersedes the ad-hoc layout in
`apps/desktop/src/App.tsx` and `apps/desktop/src/styles.css`.

## 1. Problem

The desktop is used primarily on a 13" MacBook Air, whose webview is roughly
1440 x 900 CSS px. The current main view does not fit that budget.

### 1.1 The shell leaks a row

`.app-shell` declares `grid-template-rows: 54px 28px minmax(0, 1fr)`
(`styles.css:39`) but renders **four** children: the chronology/edit-plan
drawer at `App.tsx:2644` lands in an implicit auto row. Whenever a project is
open, the drawer's tab strip costs ~68px of workbench height even with both
panels closed.

Measured height budget today:

| Row | Height |
| --- | ---: |
| `.topbar` | 54 |
| `.status-line` | 28 |
| `.workbench` | ~750 |
| `.chronology-drawer` (implicit) | ~68 |

### 1.2 Notes sits below the fold

The inspector is a 320px-wide, ~750px-tall scroll container holding ~1387px of
content — 1.85 screens. Section heights, measured from the stylesheet:

| # | Section | Height | Cumulative top |
| --- | --- | ---: | ---: |
| 1 | Clip log title | 103 | 0 |
| 2 | Labels | 121 | 103 |
| 3 | Frame orientation | 107 | 224 |
| 4 | Description | 230 | 331 |
| 5 | Transcript | 270 | 561 |
| 6 | **Notes** | 200 | **831** |
| 7 | Queue visibility | 60 | 1031 |
| 8 | Source | 112 | 1091 |
| 9 | Clock event | 184 | 1203 |

The fold is at 750. With a transcript present, Notes — the element the user
identifies as the most important in the application — starts 81px below the
fold and is never visible without scrolling.

This is an ordering failure, not a tuning failure: the two fields the human
authors (Notes, Labels) are separated by three the machine authored
(orientation, description, transcript).

### 1.3 Chrome, duplication, and defects

- Nine `.inspector-section` blocks cost ~360px in padding, micro-labels and
  borders before any content renders.
- `.transport-bar` (46px) duplicates play, timecode and mute from
  `.viewer-controls`.
- The clip count appears three times: `.session-readout`, `.count-badge`, and
  the `"273 clips ready"` message written once at `App.tsx:319` and never
  cleared, in a permanently reserved 28px row.
- `.story-labels` and `.story-canvas` hardcode `repeat(5, 22px)`. Fewer cameras
  leave dead rows; a sixth camera is clipped by `overflow: hidden`.
- `.sync-indicator` references `--border-default`, `--color-text-muted`,
  `--color-accent` and `--color-surface-raised`. **None are defined in
  `:root`.** Invalid `var()` is invalid-at-computed-value-time, so `border`
  falls back to its initial value (no border) and `color` inherits at full
  brightness. The pill renders unlike every other control in the topbar.
- `.button:disabled { opacity: .42 }` dims the whole element including its
  border, so the disabled "Proxies ready" button and the enabled `▾` beside it
  show different outlines despite sharing a class.
- `.project-switcher select` uses `--line`; adjacent `.button` uses
  `--line-strong`.
- `.calibration-*`, `.edit-plan-*` and `.sync-diag-*` use rem units and a
  Catppuccin palette (#1e1e2e, #313244, #cdd6f4) while the rest of the app uses
  px and the graphite/amber palette.
- The type scale has five steps below 13px, bottoming out at 8px
  (`.viewer-controls`, `.queue-index`, `.order-track`, `.timeline-clip`).

## 2. Governing rules

These are invariants, not preferences. New work is checked against them.

1. **The right rail holds human-authored content only.** Notes, labels, title,
   and the removal control. No read-only field, no machine output, no
   metadata may be added to it.

   Exactly two exceptions are permitted, and both are narrow enough to state
   in full:
   - the **AI suggested-label strip**, because its interaction is authoring
     (promote a suggestion to a real label), not reading;
   - the **`⚠` unusable chip** beside the title, because it prompts a human
     decision the rail exists to capture. It is a 10px chip that reveals its
     reason on click; the reason text itself lives in the Analysis tab.

   Anything else machine-authored belongs in the centre column. This rule
   exists because priority *ordering* decays — it is how the pane reached its
   current state. A categorical rule can be checked.

   In Focus (§3.9) there are no columns, so the split is expressed **per card**
   instead: human cards (notes, labels) carry a solid border, machine cards
   (transcript) carry the dim dashed treatment already used for AI label
   chips. The intent is unchanged — machine content may sit beside the
   authoring surface but never inside it.

2. **Machine output lives in the centre column**, under the player, in a
   tabbed shelf. Transcript segments are seek controls and belong near the
   transport. At 740px a transcript line fits on one line; at 400px it wraps to
   two or three.

3. **The Notes field has a fixed height and a fixed position within a
   presentation.** It never resizes, never moves, and never scrolls out of
   view. Expanding or collapsing anything else must not shift it.

   Review and Focus (§3.9) are two presentations, each with its own fixed
   notes geometry. Moving between them is the single permitted change, it is
   always user-initiated by an explicit keystroke, and it preserves caret
   offset, selection and scroll position. Nothing else may move the field.

4. **Permanent pixels are earned by frequency of interaction per clip**, not by
   abstract importance:
   - every clip → permanent, sized to its interaction
   - some clips, needed instantly → one keystroke, zero navigation (a shelf tab)
   - a few times per project → a glyph plus popover
   - once per session → menu, not a persistent widget
   - once per project → its own full-window mode

5. **A number already visible elsewhere gets no second permanent home** unless
   the two homes serve different scanning tasks.

6. **Batch operations never block review.** No modal progress, ever.

## 3. Layout

Total 1440 x 900. Dividers are 1px hairlines drawn over region boundaries, so
region dimensions sum cleanly.

```
+-- topbar ------------------------------------------- 1440 x 44 --+
| Japan 2026 v | Review Chronology Edit plan | progress | (o) @ ?  |
+---- 2px completion hairline --------------------------------------+
| QUEUE 300      | VIDEO 740 x 640            | JUDGMENT 400 x 700  |
|                | +------------------------+ | #118  Ramen alley v |
|  search    32  | |                        | | NOTES     Saved 2s  |
|  filters   30  | |        740 x 640       | | +-----------------+ |
|  +----------+  | |                        | | | 376 x 400       | |
|  | 12 rows  |  | +------------------------+ | | 15/22, 18 lines | |
|  | @ 52px   |  | > <15 >15 0:12/1:47 --*-- | +-----------------+ |
|  |   638    |  | mute  rot  PROXY  full 32 | LABELS              |
|  |          |  | Transcript 24|Desc|Anal|File  [+ add label   ]  |
|  +----------+  | (shelf collapsed)      28 | ramen x  night x    |
+-- STORY RIBBON ------- full width, 5 lanes -------- 1440 x 156 --+
```

Arithmetic: `44 + 700 + 156 = 900`; `300 + 740 + 400 = 1440`;
centre `640 + 32 + 28 = 700`;
judgment `12+32+8+18+400+10+18+28+120+8+30+16 = 700`;
queue `32 + 30 + 638 = 700`.

### 3.1 Shell

`.app-shell` becomes `grid-template-rows: 44px minmax(0, 1fr) auto` with
**exactly three children**: topbar, workbench, ribbon. The `.status-line` row
is deleted. The chronology drawer leaves the shell entirely (§6).

`.workbench` becomes `grid-template-columns: 300px minmax(0, 1fr) 400px`.

Ribbon height is `56 + min(laneCount, 5) * 20`, so a two-camera project yields
96px and returns 60px to the main row. The main row is `1fr` and absorbs the
difference.

The three columns absorb a taller main row differently, and this is deliberate:

- **queue** — the list viewport is `1fr` and grows, showing more rows;
- **centre** — the video stage is `1fr` and grows; transport and tab strip are
  fixed;
- **judgment** — the stack is fixed at 700px and does **not** grow. Spare
  height becomes bottom padding.

The judgment panel deliberately leaves the space empty rather than
distributing it, because growing the notes field would violate rule 3: its
height and its distance from the top of the viewport must be identical for
every clip in every project.

The layout table in §3 states the five-lane case (main row 700), which is the
target project shape.

Below 1200px width: queue 260, judgment 340. The existing `min-width: 980px`
on `html, body, #root` stands.

### 3.2 Topbar — 1440 x 44

Left: project name as a dropdown (footer items: Open catalog…, New project…),
then a visible segmented control `Review · Chronology · Edit plan`.

The segmented control stays visible rather than collapsing into the project
menu: three modes is few enough to show, and hiding them makes them read as
settings.

Centre: the **operation slot**, empty when idle. Empty space here is
intentional — it is the only calm region on screen. Three states in priority
order:

1. Batch running — `Analyze · stage 2/3 local speech · 118/273 · 14m left ·
   6 workers · $0.41 / $2.00 [Cancel]`, with a 2px determinate bar beneath.
   Cancel lives here and nowhere else.
2. Error — amber, persists until dismissed.
3. Toast — `Proxy ready · clip 118`, 4s auto-dismiss.

This is the entire replacement for `.status-line`. The idle
`"273 clips ready"` message is deleted.

Right: sync dot (already correctly hidden when unpaired), settings, `?`.

Media generation keeps its split button. When `missingProxyCount === 0` the
main button is **not rendered** and the control collapses to the `▾` menu —
this removes the disabled-border mismatch at its source rather than restyling
it, and reclaims ~110px. When work is outstanding, the count is state worth
seeing, so the button returns.

`Pair with server` moves into Settings, where `Connect to server` already
lives. `.session-readout` is deleted.

**Completion hairline** — 1440 x 2 flush at the topbar's bottom edge, showing
the fraction of clips with at least one label or note. Ambient progress for
2px, replacing the 180px readout.

### 3.3 Queue — 300 x 700

- Search 284 x 26, `/` focuses. Scope is the current filter.
- Filter segmented control 284 x 26, counts inline in the labels:
  `All 273 · ⚠ 12 · Removed 8`. Counts get no separate line.
- List viewport 284 x 638 — 12 rows at 52px plus a 14px peek.

Row: 22px position numeral, 72 x 40 thumbnail, title at 13/18 (one line,
ellipsis), meta line at 11px (`A7 · 1:47 · 4 labels`), and a 14px flag column
stacking up to three 6px marks — unusable, has labels/notes, original offline.
Removed rows get a hatch over the thumbnail at 55% opacity. The current row
keeps a 2px accent bar and 3 rows of lookahead.

52px is kept over a tighter 44px: 12 rows versus 13 is negligible, and
recognisability across 273 clips is not. Recognition is the queue's job;
ordering is the ribbon's.

`.shortcut-strip` is deleted — it lives in the `?` modal.

### 3.4 Video stage — 740 x 640

Zero padding, black, no border. Rotation applied at display time; portrait
pillarboxes without crop. Buffering is a 2px indeterminate hairline across the
canvas top — no overlay, nothing covering the frame.

Conditional states: a 20px amber strip when the clip is removed
(`Removed from project — R to restore`); the existing placeholder with
`Make proxy` when neither proxy nor original is available.

### 3.5 Transport bar — 740 x 32

Permanent. `.viewer-controls` and its auto-hide rule
(`.viewer-stage.is-playing:not(:hover) .viewer-controls { opacity: 0 }`) are
deleted, and so is `.transport-bar`; this single bar replaces both. A scrubber
that vanishes during playback is unaimable, and the overlay covered the frame.

Contents, 24px targets on a 32px bar: play/pause · −15 · +15 · `0:12 / 1:47`
(11px tabular) · scrubber · mute · rotation glyph · PROXY/ORIGINAL badge
(PROXY dim, ORIGINAL amber) · `⤢`. Previous/next clip buttons are added here
for mouse use; `J`/`K` are unchanged.

`⤢` enters Focus (§3.9) with all overlays hidden — the pure-frame state. The
`.viewer-stage:fullscreen` rules are deleted; Focus is the only fullscreen
concept in the application.

The scrubber carries a diamond marker at `bestMomentStartMs` (`B` jumps to it)
and thin ticks at transcript segment starts. This replaces the
`Jump to best moment · 0:42` button at `App.tsx:1909` — a timestamp printed as
text is inert; as a marker it is a control.

**Rotation** is one 24px glyph rendering the current orientation as a small
frame outline, gaining a 1px amber outline when overridden. Click opens a
128 x 96 popover: `Auto (90°) · 0° · 90° · 180° · 270°`. Permanent cost 576px²,
down from 107px of full-width section. `.orientation-thumbnail` and its
pseudo-element artwork are deleted.

### 3.6 Evidence shelf — 740 x 28 collapsed, 740 x 274 open

**Collapsed by default.** The 28px tab strip is always visible and carries
badges, so nothing is hidden; `D` toggles. Open, the shelf takes 246px from the
video stage (640 → 394).

Tabs are 13px text with a 2px active underline. No cards, no borders, no
section headers inside the content — the tab is the header.

| Tab | Content, 740 x 246 |
| --- | --- |
| Transcript *n* | Status line at 11px only when status is not `speech`. Otherwise a language chip plus 22px segment rows: 11px tabular timestamp in a 44px gutter, 13/18 text. 11 visible, scrolls, click seeks. |
| Description | The machine paragraph at 13/18, editable in place, autosaving. An `edited` tag appears once a human has touched it, so re-analysis never silently reverts it. |
| Analysis | Two-column key/value at 12/16: usable verdict and reason, camera movement, AI labels, OCR text. Footer: model, date, staleness, `Re-analyze this clip`. |
| File | Two columns at 12/16: filename, camera, codec, dimensions, fps, duration, size; camera clock and corrected time shown as a pair **with the delta**; original and proxy availability; `Reveal in Finder`, `Make proxy`. |

Badges: transcript segment count; an amber dot on Analysis when the analysis is
stale or `usable` is false. The active tab persists across clips — it is a
mode, not per-clip state.

### 3.7 Judgment panel — 400 x 700

Human-authored content only (rule 1). Content width 376px.

| Element | Height |
| --- | ---: |
| pad top | 12 |
| clip header | 32 |
| gap | 8 |
| NOTES micro-header | 18 |
| **notes textarea** | **400** |
| gap | 10 |
| LABELS micro-header | 18 |
| label input | 28 |
| chip well | 120 |
| gap | 8 |
| AI suggested strip | 30 |
| pad bottom | 16 |

- **Clip header** — `#118` at 11px, title at 17/22 editable inline (`I`), an
  `edited` tag once human-touched, an amber `⚠` chip revealing `unusableReason`
  when usable is false, and a `⌄` menu: Remove (`R`) / Reset title to machine
  value / Copy clip path. This absorbs the entire 60px `.remove-section`.
- **NOTES header** — `NOTES` at 10px caps with a save state at the right
  (`Saved 2s ago` / `Saving…` / `Unsaved`). No save button.
- **Notes textarea** — 376 x 400, 15/22, 12px inner padding, 18 visible lines,
  internal scroll. Debounced autosave at 600ms plus save on blur and on clip
  change. `N` focuses. **15px is used nowhere else in the application**: the
  hierarchy is stated in the type scale rather than in chrome.
- **Label input** — 376 x 28, one line, 13px, placeholder `+ add label`. Enter
  commits and retains focus; comma commits; Esc blurs; Backspace on an empty
  input removes the last chip. The separate `Add` button
  (`.label-add-button`) is deleted — Enter and comma already commit at
  `App.tsx:1823`. The two-line empty-state sentence is deleted.
- **Chip well** — 376 x 120, chips at 12/16, wraps ~4 rows then scrolls.
- **AI suggested strip** — 376 x 30, dashed dim chips prefixed `ai`. **Click
  promotes a suggestion to a human label**, moving it into the well as a solid
  chip. Hidden when there are no suggestions. Today `aiLabels` render as inert
  cyan chips at `App.tsx:1907`; making them actionable is the reason this one
  machine element is permitted in the rail.

### 3.8 Story ribbon — 1440 x 156 (5 lanes)

- **Control row, 22px** — left: the current clip's context breadcrumb
  (`Kansai · Osaka · UTC+9`). Right: zoom `−` / value / `+` (`[` `]`), `C`
  center, `T` collapse. The `N clips in relative order` line is deleted; it is
  the third copy of that count.
- **Trip context band, 20px** — contiguous runs of phase / location / timezone
  / story beat as flat segments with 11px inline labels, degrading to
  colour-only blocks below 60px of run width. A timezone change draws a 1px
  vertical rule through the full ribbon height — the visual explanation for
  disagreeing clocks.
- **Position numeral row, 14px** — 11px tabular, thinned to every 5th below
  16px per clip.
- **Lanes** — 96px sticky gutter with the camera name at 11px, then the track.
  Lane height 20px. `repeat(5, 22px)` in `.story-labels` and `.story-canvas` is
  replaced by `repeat(var(--lane-count), 20px)`. Past 5 lanes the lane stack
  scrolls internally with the gutter fixed; `Shift+T` expands the ribbon to
  236px **as an overlay** across the bottom of the main row, so the layout
  underneath never reflows.
- Current clip: 2px ring plus a full-height 1px playhead rule.
  Clock-corrected clips keep their distinct treatment. Removed clips render at
  30% with a hatch.
- **`T` collapses the ribbon to 28px.** Reclaimed height goes to the video
  stage and the queue list. The notes field does not grow — rule 3.

At full width the ribbon shows ~21 clips at the default zoom versus ~12 today.

### 3.9 Focus — the notes-writing presentation

Focus fills the window with the frame and floats only what the writing flow
needs on top of it. It exists because writing a note is the one task where the
Review layout's three-column split works against you: the video is 740px in a
1440px window, and your attention is on one field.

**Focus is a presentation of Review, not a fourth mode.** It carries the same
clip, the same queue order, the same filter and search. `⌘1`/`⌘2`/`⌘3` from
inside Focus leave it and go to that mode. Chronology and Edit plan have no
Focus presentation.

#### Entering and leaving

`F` toggles Focus from Review and back. `Esc` also leaves.

The transition preserves, in both directions: caret offset and selection in the
notes field, notes scroll position, video `currentTime`, play/pause state, mute
state, and the selected clip. Dropping into Focus mid-sentence and coming back
must land the caret on the same character. This is the whole point of the
feature — a transition that costs you your place is one you stop using.

**Focus does not request macOS fullscreen.** It fills the app window only.
Native fullscreen animates to a separate Space over roughly 700ms, which
defeats a toggle meant to be pressed dozens of times per session. The ~56px of
menu-bar height it would recover is not worth that.

#### Layout — 1440 x 900

```
+------------------------------------------------------------------+
| #118  Ramen alley night walk            Saved 2s    118/273   36 |
+------------------------------------------------------------------+
|                                                                  |
|          video, full-bleed, contain, centred                     |
|          16:9 -> 1440 x 810      9:16 -> 506 x 900               |
|                                              +-----------------+ |
|                                              | LABELS      180 | |
|                                              | [+ add label  ] | |
|  +--------------------------------+          | ramen x night x | |
|  | NOTES              Saved 2s    |          +-----------------+ |
|  |                                |          | TRANSCRIPT      | |
|  |   568 x 364, 15/22, 16 lines   |          | (dashed = machine)|
|  |                                |          | 0:04 we finally | |
|  |          600 x 420             |          | 0:09 it's like  | |
|  +--------------------------------+          |      360 x 568  | |
|                                              +-----------------+ |
+------------------------------------------------------------------+
| > 0:12/1:47 -----*----------------------------- mute          28 |
+------------------------------------------------------------------+
| Kansai . Osaka             117 118 119 120 121 122 ...        44 |
+------------------------------------------------------------------+
```

Video is the background layer, `object-fit: contain`, centred, full-bleed. Every
other element is an overlay card with a translucent panel background and
`backdrop-filter: blur(8px)`, matching the treatment `.inspector-title`
already uses.

| Overlay | Geometry | Default |
| --- | --- | --- |
| Top strip | 1440 x 36, top edge | on |
| Notes card | 600 x 420, 24 left, 16 above the bottom stack | on |
| Right stack | 360 x 760, 24 right, y 52–812 | **off** |
| Transport | 1440 x 28, above the ribbon | on |
| Ribbon overlay | 1440 x 44, bottom edge | on |

Notes card internals: 16 pad, 18 header, 6 gap, **568 x 364 textarea**, 16 pad
= 420. At 15/22 that is 16 visible lines at a ~75-character measure, against
Review's 18 lines at ~47. Same type size, wider measure, comparable capacity —
Focus trades two lines of height for a measure much closer to a comfortable
reading width for prose.

Right stack internals: labels card 360 x 180, 12 gap, transcript card
360 x 568 = 760.

- **Top strip** — `#118`, the clip title (editable in place with `I`), the save
  state, the `⚠` chip when unusable, and `118 / 273`. Nothing else.
- **Notes card** — identical behaviour to §3.7: 15/22, debounced autosave at
  600ms plus save on blur and on clip change, `N` focuses. Entering with `F`
  focuses it immediately, since that is what you came for. Entering with `⤢`
  does not, because that path starts with every overlay hidden; pressing `H`
  from there reveals the overlays and focuses the field.
- **Right stack** — off by default; the user called it optional and the
  writing flow does not need it. Labels card is the §3.7 input and chip well
  verbatim. Transcript card follows the playhead, highlighting the current
  segment and scrolling to it, and clicking a segment seeks. It carries the
  dashed machine treatment per rule 1.
- **Transport** — play/pause, `0:12 / 1:47`, scrubber with the best-moment
  diamond and transcript ticks, mute. No rotation glyph, no PROXY badge, no
  fullscreen control — those are per-project or per-clip setup, not writing.
- **Ribbon overlay** — 6 pad, 14px trip-context strip (colour runs plus the
  current phase name at the left), 18px **single merged lane** with position
  numerals, 6 pad. Camera lanes are collapsed to one: in this flow the
  question is "where am I in the trip", not "which camera". The current clip
  keeps its ring, and clock-corrected clips keep their marker. Click still
  selects.

#### Keys inside Focus

`T` and `D` keep the meanings they have in Review, so there is one thing to
remember rather than two:

| Key | Effect |
| --- | --- |
| `F`, `Esc` | back to Review |
| `T` | toggle the ribbon overlay |
| `D` | toggle the right stack |
| `H` | toggle **all** overlays — the pure-frame state |
| `J` / `K`, arrows | previous / next clip, without leaving Focus |
| `Space`, `B`, `L`, `N`, `I`, `R` | unchanged |

`H` replaces the old `.viewer-stage:fullscreen` special case. The `⤢` control
in the Review transport bar (§3.5) enters Focus directly in the `H` state — one
control, one mode, two states, instead of two overlapping fullscreen concepts.

Overlay visibility is **never** driven by hover or by playback state. It
changes only on an explicit keystroke, for the reason given in §3.5: a control
that appears on mouse-entry is one you cannot aim at, and a notes field that
vanishes because you stopped typing is worse.

## 4. Where every item lives

### Editable per-clip

| Item | Home | Change |
| --- | --- | --- |
| notes | Judgment, 376 x 400 fixed | Promoted; largest element on screen |
| labels | Judgment, 28px input + 120px well | Promoted; input no longer a large field |
| title | Judgment header, inline, `edited` tag | Kept |
| description | Shelf → Description tab, inline edit | Demoted from a permanent 230px block |
| removed | `R`, header `⌄` menu, queue hatch, canvas strip | `.remove-section` deleted |
| rotationDegrees | 24px transport glyph → popover | Demoted from 107px |

### Read-only reference

| Item | Home |
| --- | --- |
| filename, camera, codec, size, dimensions, fps | File tab |
| duration | File tab and the queue meta line |
| capturedAtRaw / bestTime | File tab, as a pair with the delta |
| original mounted / proxy present | PROXY-ORIGINAL badge, File tab, queue flag |
| queue position | Judgment header, queue numeral, ribbon numeral row — three places, because it is the shared coordinate between them |

### Machine analysis

| Item | Home |
| --- | --- |
| usable / unusableReason | `⚠` chip beside the title, Analysis tab, queue flag, filter view |
| cameraMovement | Analysis tab |
| aiLabels | Judgment panel, click-to-promote |
| bestMomentStartMs | Scrubber diamond, `B` |
| ocrText | Analysis tab |
| model, stale flag | Analysis tab footer, amber dot on the tab |
| transcript status / language / segments | Transcript tab |

### Deleted

1. `"273 clips ready"` (`App.tsx:319`) — restates the filter count.
2. `.status-line` — a permanently reserved 28px row, usually stale.
3. `.session-readout` — replaced by the 2px hairline.
4. `.transport-bar` and `.viewer-controls` — replaced by one permanent bar.
5. `.remove-section` — folded into the clip header.
6. `.label-add-button` and the labels empty-state sentence.
7. `.orientation-thumbnail` pseudo-element artwork.
8. `.shortcut-strip` in the queue — it is in the `?` modal.
9. `Save` buttons anywhere; autosave plus a state word replaces them.

## 5. Density system

### Type scale — six sizes, no exceptions

| px / line-height | Weight | Use |
| --- | --- | --- |
| 10 / 12 | 600, caps, +6% tracking | `NOTES`, `LABELS`, `PROXY`, save state, `edited` |
| 11 / 14 | 400 | metadata, timestamps (tabular), ribbon numerals, queue meta |
| 12 / 16 | 400 | chips, key/value rows, filter labels |
| 13 / 18 | 400 | queue titles, transcript segments, description body, tabs, label input |
| **15 / 22** | 400 | **the notes textarea, and nothing else** |
| 17 / 22 | 500 | clip title in the judgment header |

The 8px and 9px steps are deleted. At 8px, `.timeline-clip` names and
`.queue-index` are texture rather than text.

### Spacing — 4px base

`2 · 4 · 6 · 8 · 12 · 16 · 24`. Nothing above 24. Region padding 12 (8 in the
queue). Within a group 6; between groups 12.

### Chrome rules

- 1px borders only, one hairline value.
- **No element has both a border and a background fill** — one or the other.
- **Maximum nesting depth 2**: a bordered element may not contain another
  bordered element.
- Radii 3px (chips, inputs, buttons) and 6px (dialogs). Nothing else rounds.
- No drop shadows except on true overlays.
- A section header is 18px, or it is a tab label, or it does not exist.

## 6. Modes

`Review`, `Chronology` and `Edit plan` are full-window modes occupying
1440 x 856 below the topbar. The topbar persists so batch progress stays
visible while calibrating.

Entry: `⌘1` / `⌘2` / `⌘3`, or the topbar segmented control. Exit: `⌘1` or Esc.

Focus (§3.9) is **not** a fourth mode and does not appear in the segmented
control. It is a presentation of Review carrying the same clip, filter, search
and queue order, toggled with `F`. Chronology and Edit plan have no Focus
presentation; pressing `⌘2` or `⌘3` from inside Focus leaves it first.

Returning to Review restores the exact clip, queue scroll position, ribbon zoom
and offset, active shelf tab, and cursor position in the notes field. Switching
modes is a camera move, not a state reset.

`CalibrationPanel` and `EditPlanPanel` move out of `.chronology-drawer` into
these modes and are restyled onto the graphite palette in px (§7).

Edit plan seeds from the clips visible under the **current Review filter and
search**, so searching, filtering, then pressing `⌘3` opens a plan seeded with
exactly what was on screen.

One chronology affordance stays in Review: when the solver has produced errors,
the ribbon control row shows a `⚠ 3 order conflicts` chip opening Chronology
mode at the first conflict. It is ~90px wide and usually absent.

## 7. Defect fixes

Carried out as part of this work, each traceable to §1.3:

1. `.app-shell` gains its correct child count; the implicit fourth row is gone.
2. Define `--border-default`, `--color-text-muted`, `--color-accent` and
   `--color-surface-raised`, or rewrite `.sync-indicator` onto the real
   palette. Audit the stylesheet for any other undefined custom property.
3. `.project-switcher select` uses `--line-strong`, matching adjacent buttons.
4. `.button:disabled` sets an explicit muted foreground instead of
   `opacity: .42`, so borders stay consistent with neighbours.
5. `repeat(5, 22px)` → `repeat(var(--lane-count), 20px)` in `.story-labels` and
   `.story-canvas`.
6. `.calibration-*`, `.edit-plan-*` and `.sync-diag-*` move from rem and
   Catppuccin to px and the graphite/amber palette.

## 8. Keyboard

Unchanged: `J`/`K` and arrows move, `Space` play/pause, `L` label, `R` remove,
`N` notes, `/` search, `?` help, `C` center, `[` `]` zoom, `Esc` cancel.

New: `⌘1` `⌘2` `⌘3` modes, `F` toggle Focus, `D` toggle evidence shelf, `T`
collapse ribbon, `Shift+T` ribbon overlay, `B` jump to best moment, `I` edit
title.

`T` and `D` carry the same meaning in Review and in Focus — collapse the
ribbon, toggle the machine-output surface — so the pair is learned once. `H`
exists only in Focus, where it hides every overlay.

**Changed — Notes.** `Enter` inserts a newline, `⌘Enter` saves, `⇧Enter` saves
and advances. This inverts the current mapping at `App.tsx:1971`. Enter-to-save
in a field sized for 18 lines of prose is hostile, and `⌘Enter`-for-newline
inverts the macOS convention. The new field invites longer notes, so the
convention must match. Every other field keeps its current behaviour.

## 9. Out of scope

Deliberately excluded from this work, to keep the layout change reviewable
against behaviour changes:

- **Label typeahead** against the project's existing label vocabulary. High
  value at this corpus size — it is what stops `night walk` / `nightwalk` /
  `night-walk` fragmenting the search index — but it is a feature. Own task.
- **Search snippets** in queue rows, with `↵` jumping to a matching transcript
  segment's timestamp. Own task.
- **Transcript auto-scroll**, highlighting the segment under the playhead and
  suspending for 5s after a manual scroll. Own task.
- **Native macOS menu bar** for once-per-session actions (project switching,
  settings, modes). Correct destination, but it needs Rust-side Tauri menu
  work, not CSS. Own task.
- Resizable splitters — rejected. One user, one screen size; muscle memory
  across 270 repetitions is worth more than fit. `T` and `D` are the two
  discrete toggles, each with a defined pixel destination.
- Contact-sheet or grid view — it does the queue's job and the ribbon's job
  worse than either. If wanted, it is a fourth mode, not a fourth panel.
- In/out points and trimming — out of scope per the product invariants;
  rough-cut selection belongs to edit planning.

## 10. Verification

- `pnpm --dir apps/desktop check` and `pnpm --dir apps/desktop build`.
- A real `pnpm dev` Tauri run at 1440 x 900 and at 1280 x 800, confirming:
  - the notes field is fully visible without scrolling on both, for a clip
    with a transcript, an analysis, and five camera lanes;
  - the notes field does not move when the evidence shelf or the ribbon is
    toggled;
  - the judgment panel does not scroll at all at 1440 x 900;
  - the ribbon renders the correct lane count for a 2-camera and a 6-camera
    project;
  - the topbar operation slot is empty at idle and shows progress, ETA, spend
    and a working Cancel during an analysis run;
  - no control in the topbar shows a mismatched outline.
- Focus (§3.9), on the same run:
  - `F` in, type a word mid-sentence, `F` out, `F` in again — the caret lands
    on the same character each time, and the video has not restarted or
    changed play state;
  - `J`/`K` advance clips without leaving Focus;
  - `H` hides every overlay and restores them; `T` and `D` toggle the ribbon
    and the right stack independently;
  - no overlay appears or disappears on mouse movement or on play/pause;
  - the notes card is focused on entry;
  - a portrait clip renders 506 x 900 without crop, with overlays still legible
    against it;
  - `⌘2` from inside Focus lands in Chronology, and `⌘1` returns to Review —
    not to Focus.
- Measured against §1: video stage 460 → 640px, ribbon width 863 → 1440px,
  right pane scroll height 1387 → 700px with no overflow.

## 11. Provenance

The layout merges two independently produced designs: one derived from an
audit of the existing implementation, and one produced clean-room by an agent
given only a functional brief and the user's verbatim complaints, with no
repository access. Eleven decisions converged without shared input, including
the full-width ribbon, modes for chronology and edit planning, deletion of the
status line, and the priority of the notes field.

The clean-room design contributed the governing rules in §2 — in particular the
categorical human-versus-machine split (rule 1) and the fixed-geometry notes
field (rule 3) — the evidence shelf, the click-to-promote AI labels, the
scrubber marker, and the permanent transport bar. The audit-derived design
contributed the defect list in §1.3 and §7, the collapsed-by-default shelf that
preserves a 640px video stage, the visible mode switcher, and the media split
button that keeps outstanding-work counts visible.

The Focus presentation in §3.9 was specified by the user after both designs
were merged. It subsumes the fullscreen control that both designs had kept as
a separate concept.
