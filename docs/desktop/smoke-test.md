# Native macOS desktop smoke test checklist

Repeatable checklist for exercising the implemented portion of the `PRD.md`
§21.1 user journey against the real native macOS application. CI cannot do
this; every row here tests something headless runners cannot reach.

The frontend half of these journeys — which command each action issues, what
the backend then holds, and what the interface shows afterwards — is now covered
automatically by the fast CUJ suite (`pnpm test:desktop:cuj`). No row below is
retired by it: each one is here for the native evidence a fake IPC destination
cannot produce — real persistence across a relaunch, real playback, a real
volume ejection, a real card. Treat the fast suite as the reason you should not
be finding *frontend* bugs here, and this checklist as the only place the native
claims are established.

One row to read carefully: row 12 expects the review position to be restored on
relaunch. The frontend records it on every selection change and never reads it
back (`docs/tasks/harness/FINDINGS.md` §2), so if that row passes, something on
the Rust side is doing it — record which.

## Preconditions

1. Build: `pnpm --dir apps/desktop tauri build` produces
   `target/release/bundle/macos/RCD VlogCutter.app`.
2. A media folder with 10–30 clips from at least two distinct cameras, mixed
   codecs (H.264 + H.265), at least one non-ASCII filename and one with
   spaces in the path. Real camera files, not synthetic.
3. A clean projects root (no prior catalogs) to start from.
4. Source volume is mounted and the app has filesystem permission.
5. macOS version, machine model, and commit SHA are recorded before starting.

## Checklist

| # | Row | Precondition / Steps | Expected observation |
| ---: | --- | --- | --- |
| 1 | Create a project | Launch app. Click "New Project", name it. | Project appears in library. Relaunch app; project still visible. Catalog file exists on disk under projects root. |
| 2 | Set projects root | Open Settings, choose projects root. | Chosen root is used for new catalogs. Existing catalog at that path is discovered. |
| 3 | Add a media folder | Click "Add Media", select folder with 10–30 clips. Wait for ingest to complete. | Asset count matches file count. Non-ASCII and space-containing filenames present and correctly displayed in the review lane. |
| 4 | Originals referenced in place | Before and after ingest: `find <source> -type f -exec stat -f "%N %z %m" {} \; \| sort | diff - <(after)`. Also check catalog's source paths via CLI. | No byte copied, no modification timestamp changed. Source paths stored in catalog, not copied. |
| 5 | Thumbnails generated | After ingest, observe the review lane. | Every asset shows a thumbnail. Progress bar and ETA update during batch generation. |
| 6 | Proxies generated | Trigger proxy generation for all assets. Interrupt by quitting mid-batch. Relaunch. | Completed proxies survive interruption. Batch resumes from where it stopped, not from the beginning. |
| 7 | Loopback media playback | Click a clip with a proxy. Press play. | Video plays with audio in the Tauri webview (not a browser). Seek to middle: frame updates. |
| 7a | Un-played first frame | Open a project with no proxies and no thumbnails. Select a clip and do not press play. | The first frame is on screen. This is what the loopback origin exists for; see `docs/tasks/desktop/MEDIA_DELIVERY.md`. |
| 8 | Rotation | Find a portrait clip (e.g. phone video). Observe orientation. Rotate 90°. Relaunch. | Initial orientation matches camera metadata. Rotation change persists across relaunch. |
| 9 | Labels, notes, reversible removal | Add a label, type a note, remove a clip. Relaunch. | Label persists. Note persists (Enter saves, Shift+Enter saves-and-advances). Removed clip appears in removed list and can be restored. |
| 10 | Keyboard-first review | Navigate with j/k (next/previous), space (toggle removal), +/- (zoom), return (centre). Type in notes field. | All keyboard shortcuts work without mouse. Typing in notes does not trigger review shortcuts (focus isolation). |
| 11 | Clock correction | Apply a clock event to one camera at a specific filename boundary. Observe displayed times before and after for both cameras. | Only the selected camera's times change at and after the boundary. Other camera is unaffected. |
| 12 | Review position resume | Navigate to a specific clip. Note its position. Quit app. Relaunch. | Review position (selected clip) restored to the same clip. |
| 13 | Volume disconnect | Eject the source volume while the app is running (during an active proxy batch for the harder case). | App reports originals offline without crashing. Proxy review of already-generated proxies continues. No new assets created. |
| 14 | Volume reconnect | Remount the source volume. | Originals recognized as available again. No corruption. No duplicate assets. In-progress batches resume. |
| 15 | Sleep and wake | Sleep the machine with app open mid-batch. Wake after 30+ seconds. | App is responsive. Job state is coherent. No data loss. |
| 16 | Relaunch integrity | After exercising all rows above, quit and relaunch. | Project state intact: clip count unchanged, labels/notes/removals preserved, rotation preserved, review position preserved, catalog file valid. |

## Transcription (CLI)

Local speech-to-text is CLI-only for now; there is no desktop surface yet. It
runs fully offline and needs no API key.

Inspect model integrity/readiness and install explicitly first. A batch
deliberately does not download models for you:

```bash
rcdvlog --catalog <c> models status
rcdvlog --catalog <c> models install
```

Installation emits progress on stderr. Ctrl-C cancels the transfer, removes the
partial file, and reports `cancelled: true`; no model is claimed installed.

| # | Check | How | Expected |
| --- | --- | --- | --- |
| T1 | Coverage before | `rcdvlog --catalog <c> transcribe status --project <p>` | `total` matches the clip count; everything in `missing`; `recipeKey` is the current STT recipe. |
| T2 | Missing model is actionable | Move a model aside, run `transcribe missing`. | Fails naming the file, its size, and the directory to put it in. Nothing is written. |
| T3 | Batch run | `rcdvlog --catalog <c> transcribe missing --project <p>` | JSON envelope on stdout; `transcribe.progress` events on stderr with a live estimate. |
| T3b | Selected batch | `rcdvlog --catalog <c> transcribe batch --project <p> --asset <a> --asset <b>` | Only those assets are considered; current transcripts are reused and progress is bounded. |
| T4 | VAD gates silence | Inspect `transcribe status` after the run. | A large fraction land in `noSpeech` — roughly two thirds on travel footage. Zero would mean the gate is broken. |
| T5 | Idempotent | Re-run `transcribe missing`. | `skipped` equals the clip count, `transcribed` is 0, and no model work happens. |
| T6 | Proxy regeneration invalidates | Regenerate one clip's proxy, then re-run `transcribe missing`. | That one clip is transcribed again; the rest are skipped. |
| T7 | Originals untouched | Unmount the source volume, then run `transcribe missing`. | Runs to completion from proxies alone. |

Run the automated end-to-end check against a real project with:

```bash
RCDVLOG_STT_FIXTURE=$HOME/Movies/VideoProjects/<project> cargo test --release -p rcdvlog-core --test stt_end_to_end -- --ignored --nocapture
```

Use `--release`. The debug profile leaves the Rust glue unoptimized (ggml itself
is always built Release by `whisper-rs-sys`) and the same corpus took several
times longer.

Reference run, 2026-08-03, M3 MacBook Air, 273 clips / 129.1 min of footage:
31.5 min wall, 131 speech, 135 `no_speech`, 7 rejected, 0 failed, 155 windows
decoded, 380 segments stored.

## Cloud clip analysis (CLI)

Clip analysis is CLI-only for now. Put the OpenRouter key in
`~/.openrouter.key` (one line) or set `OPENROUTER_API_KEY`. Analysis reads
proxies only and may include a current accepted transcript; originals may be
offline.

| # | Check | How | Expected |
| --- | --- | --- | --- |
| V1 | Coverage before | `rcdvlog --catalog <c> analyze status --project <p>` | `total` matches the clip count and stale/missing observations are reported without a network request. |
| V2 | Budget bound | Set `RCDVLOG_VLM_MAX_COST_USD` to a deliberately tiny amount and run `analyze missing`. | The batch stops before starting another request once returned usage reaches the bound. |
| V3 | Batch run | Set a suitable cost bound, then run `analyze missing --project <p>`. | JSON envelope on stdout; `analyze.progress` events on stderr; generated title, description, AI labels, and a quality-10 representative thumbnail are persisted. |
| V4 | Human ownership | Add a human note and human label before analysis and label consolidation. | The note and human label remain byte-for-byte unchanged. Consolidation remaps AI-owned labels only. |
| V5 | Idempotent | Re-run `analyze missing`. | Every current clip is skipped and no provider call occurs. |
| V6 | Proxy regeneration invalidates | Regenerate one proxy, then run `analyze status`. | That observation becomes stale; the rest remain current. |
| V7 | Originals untouched/offline | Unmount the source volume, then analyze a clip with a proxy. | Analysis succeeds from the proxy; source files are unchanged. |
| V8 | Consolidate labels | Run `labels consolidate --project <p>`, then `labels vocabulary --project <p>`. | One text-only call produces a bounded mapping; vocabulary counts reflect operation-backed AI-label remaps. |

Live controls and a real-project batch are ignored by default:

```bash
RCDVLOG_VLM_CONTROL_FRAME=/path/to/real-q1-thumbnail.jpg \
  cargo test -p rcdvlog-core --test vlm_controls -- --ignored --nocapture
```

```bash
RCDVLOG_VLM_FIXTURE=/path/to/project \
RCDVLOG_VLM_MAX_COST_USD=1 \
  cargo test -p rcdvlog-core --test vlm_end_to_end -- --ignored --nocapture
```

Reference run, 2026-08-04, M3 MacBook Air, 273 clips: 273 current, 0 failed,
0 rejected, 131 transcript-fused, one Qwen-to-Gemini fallback, $0.02732851
stored analysis cost. Twelve black/noise/blur negative controls passed for
$0.000505. Label consolidation remapped 149 assets into six labels.

## After each run

1. Record the build path, commit SHA, macOS version, and machine model.
2. Record the source folder path, clip count, and cameras present.
3. For every row: result (`pass`, `fail`, or `not_implemented`) and evidence
   file reference (screenshot or CLI output).
4. For every `fail`: an `SD-n` defect entry with reproduction steps, observed
   vs. expected behaviour, and a reproducibility note (does it happen on a
   second launch from a clean projects root?).
5. Verify originals were untouched: `diff` of pre-ingest and post-test file
   stats on the source folder.
