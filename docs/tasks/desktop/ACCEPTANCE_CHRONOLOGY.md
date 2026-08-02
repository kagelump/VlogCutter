# Antarctica chronology acceptance — automated evidence

Supports D01-T018. **This document records the automated half of the gate. The
manual half is not done.** See "Verdict" at the bottom.

The footage path is never recorded in this repository. It is supplied at
invocation as `$FOOTAGE_ROOT` (corpus) or `$CATALOG_PATH` (the ingested
catalog).

## The corpus

A three-camera Antarctica trip, ingested in full:

| Camera | Filename shape | Clips |
| --- | --- | ---: |
| DJI Pocket 3 | `DJI_<14-digit timestamp>_<4-digit counter>_D.MP4` | 420 |
| Insta360 Ultra Go | `VID_<8-digit date>_<6-digit time>_<counter>.mp4` | 71 |
| Sony A7C | `C<4-digit counter>.MP4` | 130 |

653 files scanned, 621 imported, 32 rejected. Every rejection was an
AppleDouble `._*` sidecar, correctly identified as not media.

## What was verified

Run:

```sh
FOOTAGE_ROOT=<corpus> cargo test -p rcdvlog-core --test camera_patterns_corpus -- --ignored --nocapture
CATALOG_PATH=<catalog> cargo test -p rcdvlog-core --test real_catalog_acceptance -- --ignored --nocapture
```

### Camera identification and counters (D01-T028)

| Camera | Clips | Distinct counters | Counter range |
| --- | ---: | ---: | --- |
| DJI Pocket 3 | 420 | 420 | 1–441 |
| Insta360 Ultra Go | 71 | 71 | 113–184 |
| Sony (C-series) | 130 | 130 | 684–899 |

**621 of 621 clips identified; 621 of 621 counters resolved.** Before D01-T028,
all 420 DJI clips resolved to the literal `"Unknown camera"` and carried a NULL
filename counter — 68% of the corpus could not be targeted by any counter-ranged
correction, and every unidentified camera collapsed into a single capture
sequence.

### Chronology (D01-T008 through D01-T017)

- 621 clips, **3 capture sequences** — one per camera, correctly separated.
- **0 clips with no derived time.**
- **0 contradictions.**
- The order is a strict total order: every adjacent pair compares strictly, so
  there are no ties anywhere in 621 clips.
- Re-solving the same facts produces an identical solution, field for field.
- Every asset belongs to a capture sequence; none is unassigned.

### Diagnostics (D01-T027)

The 32 rejected sidecars and the weak Sony pattern match surfaced as **three
durable rows**, not 162:

| Severity | Kind | Occurrences |
| --- | --- | ---: |
| warning | `ingest.unreadable_media` (Insta360 folder) | 18 |
| warning | `ingest.unreadable_media` (DJI folder) | 14 |
| info | `camera.weak_pattern_match` (`sony-c-series`) | 130 |

This is the D01-T005 smoke-run gap closed directly: those 14 DJI rejections were
previously discoverable only by reading CLI stdout as it scrolled past.

### Edit plan and export (D01-T019 through D01-T022)

A 12-clip plan built from the solver's order:

- Validation reported no blocking errors; only `edit_plan_proxy_missing`
  warnings, which are correct — no proxies had been generated for this run.
- The OTIO document is byte-stable across 8 serializations.
- Every `target_url` references a camera original under the corpus root; none
  references a proxy.
- Clip identifiers are `<revision_id>:<position>`, stable across re-export.
- Rational time throughout, at the clip's real rate (59.94).

## Verdict

**Not accepted — the manual gate has not been run.**

D01-T018's principal invariant is that "the real Antarctica trip footage can be
brought to a chronology **the owner judges correct**, using only the shipped
calibration workflow." Everything above is machine-checkable and passes. None of
it is the owner's judgement.

Specifically not yet done:

1. The owner has not reviewed the derived order against their memory of the
   trip. The corpus solves cleanly, but "cleanly" means internally consistent —
   the cameras' clocks were all believed as-is, because no clock epoch or trip
   phase has been entered yet. If a camera's clock was wrong, this run cannot
   tell.
2. No correction was made through the shipped desktop UI. The calibration
   surface exists and its logic is tested, but the invariant is about a person
   completing the workflow, and that has not happened.
3. No trip phases were entered, so every clip currently displays as UTC. The
   date-line and daylight-saving behaviour is tested against fixtures, not
   against this trip.

This document is updated with an `accepted` or `rejected` verdict once the owner
runs the workflow.
