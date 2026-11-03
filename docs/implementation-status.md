# Implementation status

Relocated verbatim from the repository-root `AGENTS.md` section 3 ("Current implementation versus PRD target") so the root file can stay a short index (issue #260). `AGENTS.md` keeps a one-line index per capability and the condensed "Important gaps" list; this file holds the full detail.

Convention: when a feature PR lands, put its detail here (or in a more focused doc under `docs/`) and add only an index line plus a link to `AGENTS.md`.

The implemented slice is intentionally smaller than the launch-critical list in `PRD.md`.

Implemented now:

- Preview encoding explicitly normalizes full-range sources to TV range, with
  versioned TV-range-v2 preview/segment identities. Historical integrity review
  recognizes measured-clock v1/v2 receipts; strict export requires the current
  renderer and unchanged verification guards.

- TV recorder cues/clock events use a durable outbox and acknowledged worklet
  tail flush; paired native-frame/performance probes preserve calibration
  evidence and suspension epochs. See `docs/tv-voice-review-recording.md`.

- Version-1 path-free TV review target export freshly verifies a full preview,
  preserves measured movie PTS and declared independent audio, and supplies a
  synthetic cloud fixture. The companion server imports it through an
  authenticated preflight/import (control schema 13, `review_targets` owner in
  `rcdvlog-server`), deduplicates by project + digest, refuses conflicting reuse
  of a viewed artifact, keeps a declared-unverified YouTube ID as a separate
  immutable snapshot, and the Chrome app has a *TV review* selection/import
  screen. Chrome voice-review recording (control schema 14, `review_sessions`)
  captures untouched PCM through an AudioWorklet on the microphone sample
  clock, buffers it in IndexedDB, uploads hashed sequenced chunks, records
  manual cues, and has the server compute coverage and gaps at finalization.
  Review-session ASR (control schema 15, `review_transcripts`, job
  `review_session.transcribe.v1`) reuses the shared speech pipeline through
  the `rcdvlog_core::review_speech` facade: local whisper or an explicitly
  authorized, ledger-capped native OpenRouter model, per-window caching that
  never rebills, recording-clock utterances with raw text plus an OpenCC s2tw
  display projection. Speaker diarization is reported as unavailable, never
  inferred. The editorial review workspace (control schema 16,
  `review_discussions`) adds append-only human corrections, grouped
  discussions with typed items (human-only decisions, enforced retractions and
  consistent acceptances), movie time only from the shared `tv_review_sync`
  mapper over calibrated cue evidence (pending without calibration), and
  a `review-report.v1` exporting core `RecordReviewNoteRequest`s with single
  reconciliation receipts; it never writes the project catalog. See
  `docs/tv-review-handoff.md`, `docs/tv-voice-review-recording.md`,
  `docs/tv-review-transcription.md` and `docs/tv-review-discussions.md`.
  `review-markers` (no catalog) writes a
  separate, create-only, self-verified marked review copy of the BLAKE3-matched
  master with audible chirp-BFSK movie-time markers (video stream copy) and
  detects CRC/export-key-checked anchors in recordings as shared `tv_review_sync`
  `acousticTimecode` anchors; see `docs/tv-review-acoustic-markers.md`.

- Historical rendered-movie integrity review has a separate read-only DTO/CLI;
  it never certifies current plan renderability. See `docs/tv-review-handoff.md`.
- External review notes have an idempotent `edit-plan feedback record-once`
  boundary using normal canonical note facts; retries never accept or apply an edit.
- `tv_soundtrack` supplies bounded local final-mix sync candidates; no hardware
  acceptance is implied. See `docs/tv-soundtrack-matching.md`.
- `tv_soundtrack_session` plans complete-reference searches, bounded resumable
  local batches and native utterance mapping. Its private derived cache is not
  a project owner; see `docs/tv-review-local-session.md`.
- `tv_review_sync` owns bounded evidence-only TV recording/movie alignment;
  CLI `tv-review sync-preview` is read-only. See `docs/tv-review-sync.md`.
- One supported CLI now includes editing, review and upload against schema 66,
  with observational `doctor --workflow rough-cut|delivery` preflight. Sparse
  stable-ID patches, explicit source-inventory picture-usage reports and unused
  candidate retrieval, agent/human decision provenance, historical-preview
  verification, audiovisual review coverage and measured local delivery bundles
  are described in `docs/rough-cut-readiness.md`. Existing source inspection
  supports bounded local deadlines and explicitly requested proxy fallback.

- One portable SQLite catalog per newly created desktop project.
- Configurable projects root, defaulting to `~/Movies/VideoProjects`.
- Project library discovery and loading an existing catalog.
- Reference-in-place recursive ingest of supported media extensions.
- A desktop smart-ingest dialog that classifies a source before mutation,
  states the project-scoped delta and resolved destination, checks copy size
  against free space, creates/rebinds named archive roots, copies removable
  cards atomically with optional full-file verification, remembers the proxy
  preference, supports cancellation/resume, and keeps a file ledger available
  after the dialog closes.
- Smart ingest shows the resolved camera before import, lets the user choose an
  existing or new project camera, uses that choice for both asset ingest and
  sequence preview, and remembers it only when card layout, filename prefix,
  and representative EXIF make/model form an unambiguous project-local key.
- Low-frequency macOS removable-card detection with a review/dismiss banner;
  it lists and counts only until the user asks to review the import.
- Sampled fingerprint deduplication within a project, with rediscovered content
  repairing an asset whose original went offline rather than being discarded.
- Verified camera `.LRF` sidecars can resolve through recorded same-asset
  alternate originals, with persisted CFR integer-reduction time mappings and
  bounded content checks. `camera-proxy frame-pack` samples that explicit camera
  representation offline; generated-proxy readiness remains separate. See
  `docs/camera-proxy-sidecars.md`.
- Verified node-local media locations: availability requires that the bytes were
  checked, so a reformatted card reusing a pathname cannot bind an old asset to
  new footage.
- ffprobe technical/timestamp metadata and ExifTool camera make/model lookup.
- Label queue, playback, free-form labels, notes, reversible removal, and keyboard shortcuts.
- Fixed camera offset events from a filename boundary onward.
- Operation-backed capture sequences, clock epochs, clip-time exceptions,
  clip-anchored trip phases, event-order constraints, a deterministic
  chronology solver, and durable diagnostics. A ribbon phase boundary follows
  its anchor clip when later camera-clock corrections change solved order.
- Read-only chronology/diagnostics CLI commands and basic desktop chronology
  and edit-plan drawers.
- Versioned edit-plan persistence and validation, explicit source-audio ranges,
  J/L placement and short audio fades in local previews (legacy omitted audio
  remains linked), identity-based audio diffs, and an explicit OTIO rejection
  until finishing can preserve these edits.
- Low-resolution preview
  generation, and deterministic pure-Rust OTIO writing.
- Local preview renders have schema-54 node-local attempt and verified encoded
  segment receipts, bounded progress/status reads, explicit cancellation, and
  opt-in byte/input verification. They are derived evidence, not synchronized
  decisions; see `docs/edit-plan-preview-artifacts.md`.
- Schema 57 adds separately labeled published token estimates to shared analysis
  accounting; estimates consume the same budget without becoming provider-reported
  charges. Existing transports retain their admission rules. See `docs/analysis-accounting.md`.
- Schema 58 adds node-local native-video carriers, request evidence and run/window
  details. The explicit direct Gemini route shares canonical policy approval,
  bounded capability pilots, reservation/dispatch/receipt accounting and the
  existing moment artifact owner. Source scopes and video/audio/transcript
  payloads are explicit; no provider/model/tier fallback or automatic resend is
  permitted. Local media preparation is not an upload permit. See
  `docs/native-video-review.md`; fake protocol tests do not establish live
  provider capability or human listening/pacing acceptance.
- Schema 59 adds canonical bounded local inspection-review sessions and step
  receipts over the existing source-inspection and trim-suggestion owners.
  Sessions freeze source/representation identity and human-range revision,
  consume durable local step/frame/media limits, and cannot write human facts,
  plans, or provider accounting. See `docs/edit-plan-inspection.md`.
- Schema 60 adds revision-bound assembled review evidence for verified whole-cut
  excerpts, declared audio mappings, and optional separately playable source
  handles. Explicit human notes flow through the existing feedback owner; this
  does not invoke a provider, make decisions, or activate plans. See
  `docs/edit-plan-context-review.md` and `docs/agent-cli.md`.
- Schema 61 adds bounded local short comparisons over two to four named whole-cut
  excerpts, with a standalone sequential player and explicit byte verification.
  Only an explicit human preference records feedback; optional retention and
  inactive draft application use the existing feedback and revision guards. See
  `docs/edit-plan-comparisons.md` and `docs/agent-cli.md`.
- Schema 62 adds bounded selected-audio integrity evidence for original and renderer-selected sound, known silent preview segments, and requested master/selected/rendered energy samples. It is local technical evidence only; it does not infer master silence from derivatives or certify speech/editorial quality. See `docs/selected-audio-integrity.md` and `docs/agent-cli.md`.
- Schema 56 adds optional immutable chapter selection intent, pacing profiles,
  evidence identities and reports. Selection reuses local moments, preserves human
  constraints and creates inactive normal plans; see `docs/edit-plan-selection.md`.
- Schema 55 adds portable revision-bound edit-review notes, proposals, human
  decisions and target mappings. Only an explicit accepted proposal can be
  applied, producing a normal inactive edit-plan draft; feedback does not
  activate plans or change chronology. See `docs/edit-plan-feedback.md`.
- Quality-1 thumbnails and 720p H.264/AAC proxies with four concurrent
  VideoToolbox workers on macOS and a one-worker software fallback elsewhere.
- Proxy-only cloud clip analysis with deterministic frame packs, transcript
  fusion, bounded observations, provider fallback, operation-backed AI labels,
  quality-10 representative thumbnails, and explicit label consolidation.
- A desktop analysis surface: one cancellable proxies/transcription/analysis
  chain with a pre-run cost projection and per-run budget ceiling, per-run spend
  accounting, generated titles as the clip's display name, transcript and
  observation review panels, transcript-aware queue search, unusable-clip
  triage, and Keychain-stored OpenRouter credentials.
- Weighted batch progress and ETA with project-persisted rate history.
- Review video served from a token-guarded loopback HTTP origin the desktop app
  binds at startup, replacing the Tauri asset protocol for video so an un-played
  clip has a frame to paint. Range-streamed from disk, resolved by asset id
  through the open catalog, and stopped with the app.
- A main view laid out for a 13" MacBook Air (1440x900). The shell is exactly
  three rows — topbar, workbench, full-width story ribbon — and two invariants
  are enforced by tests rather than by review: the right rail holds only
  human-authored content (notes, labels, title, removal, plus the AI
  suggestion strip and the unusable chip), and the notes field has fixed
  geometry that nothing else may move. Machine output lives in a tabbed
  evidence shelf beneath the player, collapsed by default. Chronology and edit
  planning are full-window modes on Cmd+1/2/3. Focus is the only fullscreen
  concept — the transport's ⤢ enters it with the overlays down and leaves it
  again, and native/DOM fullscreen is gone. A Focus presentation (F) fills
  the window with the frame and floats the notes card, preserving the caret and
  the playback position because it restyles the mounted tree rather than
  replacing it. See `docs/superpowers/specs/2026-08-04-desktop-review-layout-design.md`.
- One owner for the open project's reads (`apps/desktop/src/session/projectSession.ts`):
  the queue, chronology overview, analysis index, readiness, and the selected
  clip's evidence, with explicit `reload`/`reloadAssets`/`reloadChronology`/
  `reloadReadiness`/`reloadEvidence` invalidation. Every read carries the
  session that started it, so a response for a project the user has left cannot
  write state — including its error and loading state. A failed queue read is
  retryable in the interface (queue heading reload control, and *Try again* in
  the empty queue) rather than only by relaunching.
- One owner for the asset write path
  (`apps/desktop/src/session/assetMutations.ts`): optimistic apply, a stated
  failure policy per mutation — `rollback` for labels, removal, and rotation;
  `retain-draft` for human-authored notes — save state, a double-submit guard
  that refuses an identical in-flight write, and error surfacing. A note whose
  save was refused stays on screen marked *Unsaved* with a *Retry* that sends
  the same draft, and survives reloads because retained drafts are written back
  over what a read returns. Judgments are still one whole-`AssetPatch` write
  through `update_asset`; the narrow single-field adapters were removed and the
  decision is recorded in `apps/desktop/src/test/harness/contract.ts`.
- Relative-order “story ribbon” with camera lanes sized from the project's
  actual camera count, horizontal scrolling, zoom, and a merged single-lane
  variant for Focus. The expanded Chronology desk uses the same camera-first
  hierarchy, with selectable capture-sequence/card runs inside each lane.
- An explicit camera-lane identity merge previews affected clips and card runs,
  then moves the source identity into an existing lane without combining its
  capture sequences or changing clock-epoch IDs. The typed operation is
  replay/baseline safe, updates Smart ingest mappings, and derives flattened
  aliases deterministically from all merge facts so later operations carrying
  an old identity and concurrent chained merges converge.
- A non-interactive JSON CLI for catalog, clip annotation, chronology, and media operations.
- Immutable typed project operations for asset membership, notes, labels, removal, rotation, and fixed clock events, with deterministic materialized views and conflict diagnostics.
- Read-only catalog/project/clip/proxy diagnostics, including the desktop's persisted last-selected catalog, project, and asset breadcrumb.
- Optional Ubuntu companion runtime with a local-SSD control catalog (schema 16), local project replicas, logical storage roots/node bindings, a global verified-content library, and scoped availability observations.
- Durable leased jobs, cancellation/retry/restart recovery, scans/schedules, full hashing, shared proxy/thumbnail generation, node-scoped ETA history, and authenticated byte-range streaming.
- Baseline format 1 and sync protocol 1.0 with bounded operation pages, per-origin cursors/acknowledgements, deterministic conflict handling, WebSocket hints, and polling recovery.
- Browser bootstrap/session auth, revocable desktop credentials, Keychain storage, CSRF/Host/Origin/proxy validation, LAN Caddy and Tailscale Serve deployment assets.
- Chrome library/project browse, add-to-project, proxy review, annotations, storage/jobs/sync/credentials administration, and stable polling fallback.
- Authenticated server CLI families plus local-only bootstrap, restore, sequence-recovery, and redacted diagnostic commands.
- Verified scheduled/manual backups of server databases with manifest-last publication, integrity/checksum validation, retention, and new-root-only restore.

Important gaps; do not accidentally describe these as complete:

- Import preflight includes a read-only, per-run capture-sequence decision with
  volume, folder, filename-prefix, counter-reset, and time-discontinuity
  evidence. The Chronology desk supports explicit split/merge and single-clip
  or clip-and-following reassignment; preflight never applies those proposals.
  A counter restart is presented on the desk as a question with two answers —
  split into a new card, or record "same card" durably — rather than as a
  problem, because swapping cards through one camera is ordinary practice. The
  "same card" answer is an operation (`captureSequenceSplitDeclined`) and is
  reversible from the desk's Card tab.
- Project deletion and cache cleanup now ship (#16). Deletion runs from the
  opening project library behind a removability audit — refusing a folder that
  holds an archive root, a referenced original, another project's rows, or any
  file the app did not create — and a typed project-name confirmation;
  `delete_project` removes trip phases before assets because their clip anchors
  are `ON DELETE RESTRICT`. Cache cleanup is a Settings storage panel that lists
  the seven cache categories with their weight, replacement cost, and what stops
  working until rebuilt; there is no single clear-cache button, and cleanup
  reads nothing about the media so it works with every drive unplugged. There is
  no general mounted-volume inventory UI. Relink now
  exists: media locations carry a verification state, ingest repairs an asset
  whose original it rediscovers, and an offline-volume diagnostic offers
  `Locate…` (CLI: `relink`). Queue rows carry a compact offline mark that
  separates *original offline, reviewable from its proxy* from *nothing to
  play*, and the selected clip's File shelf reaches the same project-wide
  relink, so repair no longer requires knowing the chronology desk's
  diagnostics exist. Removable-card detection and archive-root rebinding
  are implemented; see `docs/tasks/import/EPIC.md` for the remaining physical
  card/volume acceptance gate.
- Full-patch writes carry an optimistic `expectedRevision`. `Asset.revision` is
  the materialized operation revision; `update_asset` refuses a patch built
  from a stale read, and the desktop recovers by re-reading and re-applying the
  user's own change on top of what the catalog now holds. The CLI takes
  `--expected-revision` and reports `stale_revision`. What this does *not* do:
  tell whose value won when both writers edited the same field — the client
  cannot distinguish its own optimistic value from the other writer's, so an
  explicit edit wins that field and the other writer's other fields survive.
  Cross-node operation sync is unchanged.
- Search is currently simple filename/camera/label/note/transcript filtering,
  not SQLite FTS. Result rows say *why* each clip matched — the field and a
  marked snippet — and Enter in the search box cycles the selected clip's
  playhead through the places the phrase was spoken, marking them in the
  transcript. Only the flattened transcript text is indexed per clip, so the
  per-row snippet has no timestamp; the timings come from the open clip's own
  transcript.
- The label field offers the project's existing vocabulary as a keyboard-first
  typeahead, and a commit that differs from an existing label only by case
  joins that label rather than opening a second one. Nothing is renamed,
  merged, or deleted: `apps/desktop/src/review/labelVocabulary.ts` is a
  read-only view over the labels already written.
- Sparse suggested key moments and a bounded preparation evidence CLI now exist;
  they do not certify arbitrary cut ranges or replace human review. Separate chapter/event/script entities remain unimplemented; optional sequence
  intent is stored with immutable edit-plan revisions. Edit-plan revisions, validation,
  preview rendering, and OTIO writing exist, but the desktop edit-plan workflow
  still lacks reordering, trimming, removal, preview playback, export history,
  and overwrite confirmation. The supported `edit-plan` CLI exposes
  list/get/create/diff/validate/activate/preview/inspect. Bounded source inspection
  stores actual decoded PTS, source/excerpt mapping, oriented frames and optional
  playable sound/waveform as derived SQLite receipts. It never accepts a range
  or changes human judgments; see `docs/edit-plan-inspection.md`. Explicit human
  picture/audio allow/exclude constraints are operation-backed, revision-guarded
  and honored by validation, preview publication and advisory utterance trims;
  see `docs/edit-plan-source-ranges.md`.
- No materialized project-local agent workspace. The CLI can report its build
  and readiness, and can explicitly bind/check the selected executable and
  catalog in `<project-root>/.vlogcutter/cli-binding.json`. This binding never
  generates or edits project instructions. Materializing `AGENTS.md`,
  `PROJECT.md`, `script.md`, `project.json`, and `edit-plan.json` into the
  video project root remains planned under issue #48 and PRD Milestone 5.
  Script revisions do not exist either.
- Trip phases and timezone display are persisted and solved, and the ribbon can
  add a location/transit phase forward or backward from a hovered clip, edit an
  existing phase, and atomically cut a new phase from either side of a filled
  segment. The Chronology desk adds identity-snapped pointer and keyboard
  boundary adjustment for both phase kinds, plus a full camera-lane/card-run
  and trip-band view. Story beats remain display-only browser-mock fields and are not
  persisted.
- The Chronology desk surfaces conflicts, explanations, diagnostics, clock
  epochs, phase boundaries, split/merge, single/following reassignment, and
  bounded solver-owned before/after impact summaries. The compact Review ribbon
  deep-links into it and Shift+T opens it with the Review zoom and viewport.
  Dedicated clip-exception and event-order-constraint controls remain domain/API
  capabilities rather than first-class desk tools.
- The desk shows the footage. A compare deck plays the focused clip with the
  clips the solver placed either side of it — either side in *solved* order,
  across cards — as stills that promote to focus on click or with the arrow
  keys. Canvas tiles are thumbnails carrying an ordinal, with the time revealed
  on hover and focus and the full value in `title` and the accessible name; a
  clip with no thumbnail keeps a text tile. The ordinal strip above the lanes is
  a time ruler, marking day changes and timezone changes, the latter because a
  trip crossing zones makes the displayed wall clock run backwards.
- Web has automated Vitest and Chrome Playwright coverage. Desktop React now has Vitest + jsdom + Testing Library component tests under `apps/desktop/src/components/__tests__/`, but there is still no automated native Tauri UI suite.
- No macOS signing/notarization or clean-machine packaging acceptance.
- The transcript shelf follows playback: the segment under the playhead is
  marked, the list scrolls only when that segment has left the viewport, and a
  manual scroll suspends following for five seconds or until *Follow playback*.
  Real scroll geometry is covered by `review/transcriptFollow.ts`'s unit tests
  rather than by a browser tier — the Vite preview has no media origin, so no
  playhead there can actually move.
- Analysis suggestions are shown and editable in the desktop, but there is no
  per-suggestion accept/reject workflow: an AI title or description is simply an
  editable field, and a forced re-run overwrites it.
- Native media playback, filesystem permission prompts, sleep/wake, and NAS-scale behavior remain manual acceptance checks. The loopback media origin's three webview-observable acceptance criteria — an un-played clip's first frame, non-zero `drawImage` alpha before `play()`, and seeking to an unbuffered region — have not been run. Resolve import returns as a later edit-planning acceptance check.
- Ubuntu/Compose, physical NFS/SMB outage behavior, Caddy/Tailscale HTTPS, native pairing/playback, and target resource/thermal acceptance remain unrun; see `docs/tasks/server/ACCEPTANCE_TASK_19.md`.

Before implementing persisted trip context, resolve the domain question of whether story beats are independent overlapping ranges or must be nested under physical phases. The current recommendation is independent ranges because narrative beats may cross a location or timezone transition.
