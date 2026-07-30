# Native macOS desktop smoke test checklist

Repeatable checklist for exercising the implemented portion of the `PRD.md`
§21.1 user journey against the real native macOS application. CI cannot do
this; every row here tests something headless runners cannot reach.

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
| 7 | Asset-protocol playback | Click a clip with a proxy. Press play. | Video plays with audio in the Tauri webview (not a browser). Seek to middle: frame updates. |
| 8 | Rotation | Find a portrait clip (e.g. phone video). Observe orientation. Rotate 90°. Relaunch. | Initial orientation matches camera metadata. Rotation change persists across relaunch. |
| 9 | Labels, notes, reversible removal | Add a label, type a note, remove a clip. Relaunch. | Label persists. Note persists (Enter saves, Shift+Enter saves-and-advances). Removed clip appears in removed list and can be restored. |
| 10 | Keyboard-first review | Navigate with j/k (next/previous), space (toggle removal), +/- (zoom), return (centre). Type in notes field. | All keyboard shortcuts work without mouse. Typing in notes does not trigger review shortcuts (focus isolation). |
| 11 | Clock correction | Apply a clock event to one camera at a specific filename boundary. Observe displayed times before and after for both cameras. | Only the selected camera's times change at and after the boundary. Other camera is unaffected. |
| 12 | Review position resume | Navigate to a specific clip. Note its position. Quit app. Relaunch. | Review position (selected clip) restored to the same clip. |
| 13 | Volume disconnect | Eject the source volume while the app is running (during an active proxy batch for the harder case). | App reports originals offline without crashing. Proxy review of already-generated proxies continues. No new assets created. |
| 14 | Volume reconnect | Remount the source volume. | Originals recognized as available again. No corruption. No duplicate assets. In-progress batches resume. |
| 15 | Sleep and wake | Sleep the machine with app open mid-batch. Wake after 30+ seconds. | App is responsive. Job state is coherent. No data loss. |
| 16 | Relaunch integrity | After exercising all rows above, quit and relaunch. | Project state intact: clip count unchanged, labels/notes/removals preserved, rotation preserved, review position preserved, catalog file valid. |

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
