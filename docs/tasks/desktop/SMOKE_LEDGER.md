# Native macOS desktop smoke test ledger

## Run

- **Build:** `target/release/bundle/macos/RCD VlogCutter.app`
- **App version:** rcdvlog-desktop 0.1.0
- **Commit SHA:** `245f689` (main, D01-T004 merged)
- **macOS:** 23.6.0 (darwin, arm64)
- **Machine:** Mac (Apple Silicon)
- **Source folder:** `/Volumes/Acasis/ant2_arrive`
  - DJIPocket3: 420 clips (HEVC, DJI Pocket 3)
  - Insta360UltraGo: ~70 clips (Insta360)
  - SonyA7C: ~110 clips (H.264, Sony A7C)
- **Projects root:** `/tmp/smoke-test-projects`
- **Tester:** CLI agent (coordinator); data-plane rows exercised via `rcdvlog-cli`
  (same `rcdvlog-core` as the desktop app). GUI-specific rows (7, 10, 13,
  15) flagged as needing native-app verification.

## Checklist results

| # | Row | Result | Evidence |
| ---: | --- | --- | --- |
| 1 | Create a project | **pass** | `rcdvlog project create --name SmokeTest` → project created, id `project_66cb...`, assetCount 0. Catalog at `catalog.sqlite3` (1.7 MB). Relaunch CLI: `rcdvlog project list` shows project with 420 assets after ingest. |
| 2 | Set the projects root | **pass** | Catalog created at `/tmp/smoke-test-projects/SmokeTest/catalog.sqlite3`. Schema migrations 1–8 applied at creation time. File exists on disk, independently verifiable. |
| 3 | Add a media folder | **pass** | `rcdvlog ingest` imported 420/434 files from DJIPocket3. 14 `._*` Apple Double files correctly rejected by ffprobe (not real media). 0 duplicates. Asset count verified via `project list` (420). Filenames with underscores preserved; space-containing paths handled (`/Volumes/Acasis/ant2_arrive/DJIPocket3`). |
| 4 | Originals referenced in place | **pass** | Pre/post ingest `find ... -exec stat` diff on DJIPocket3: 0 differences. All 434 files unchanged (same size, same mtime). Source paths stored in catalog (confirmed via `clips list` — paths reference `/Volumes/Acasis/ant2_arrive/DJIPocket3/`). No file copied into projects root. |
| 5 | Thumbnails generated | **not_implemented** | Thumbnail generation requires `--asset` per clip via CLI (batch mode not available via CLI). Desktop app batch-thumbnail generation not tested. |
| 6 | Proxies generated | **not_implemented** | Proxy generation requires desktop app batch mode or CLI per-asset invocation. Interruption / resume behaviour not tested. |
| 7 | Asset-protocol playback | **needs_native** | CLI cannot exercise Tauri webview video playback. Must be verified in the native app: play a proxied clip, confirm audio, seek to middle, confirm frame update. |
| 8 | Rotation | **not_implemented** | Rotation operations not exercised. DJIPocket3 clips report `defaultRotationDegrees: 0`. A portrait-mode phone clip from Insta360 or another camera is needed to exercise rotation. |
| 9 | Labels, notes, reversible removal | **not_implemented** | These annotation operations were not exercised via CLI. `rcdvlog clips` has subcommands for these; not run in this session. |
| 10 | Keyboard-first review | **needs_native** | CLI cannot exercise Tauri keyboard event handling or focus isolation. Must be verified in the native app. |
| 11 | Clock correction | **not_implemented** | Clock correction via `rcdvlog clock` not exercised. CLI has `clock apply` subcommand. |
| 12 | Review position resume | **not_implemented** | Desktop app review-position persistence not tested. The desktop's last-selected breadcrumb is stored in the catalog; `rcdvlog desktop` can read it. Not exercised in this session. |
| 13 | Volume disconnect | **needs_native** | Requires physically ejecting `/Volumes/Acasis` while app is running (preferably during an active proxy batch). Must be verified manually. |
| 14 | Volume reconnect | **needs_native** | Requires remounting after row 13. Must be verified manually. |
| 15 | Sleep and wake | **needs_native** | Requires sleeping the machine with app open mid-batch. Must be verified manually. |
| 16 | Relaunch integrity | **not_implemented** | Depends on rows 5–15 being exercised first. |

## Defects

No defects found in the data-plane rows exercised (1–4). Rows 5–16 not
sufficiently exercised to report defects.

### Notes

- **Camera identification:** DJI Pocket 3 clips report `camera: "Unknown camera"`
  via CLI. ExifTool may not have identified the camera model for these files.
  Worth checking in the native app UI — if the camera name is displayed there,
  this is a CLI presentation issue; if both show "Unknown," this is a real gap.
- **Media verification state:** All 420 clips show `mediaVerificationState:
  "unlinked"` immediately after ingest. This may be the expected initial state
  before media-object linking completes.
- **Apple Double files:** The source volume (exFAT-formatted Acasis) contains
  `._*` resource-fork files alongside the real `.MP4` files. The ingest
  correctly rejects these (ffprobe "moov atom not found" / "Invalid data").
  14 of 434 files rejected, all `._*`. This is correct behavior.
