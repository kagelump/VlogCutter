# Desktop analysis surface: running and reviewing STT/VLM in the app

Status: approved 2026-08-04. Supersedes nothing; extends
[the clip-analysis design](2026-08-03-stt-vlm-clip-analysis-design.md) with the
desktop surface that design deliberately left out.

## Problem

Local transcription and bounded cloud clip analysis both ship, but they are
reachable only from `rcdvlog transcribe` and `rcdvlog analyze`. The catalog
already stores everything the desktop would need — transcripts, observations,
AI-owned labels, and AI-written titles and descriptions — and none of it is
readable from the app. `AGENTS.md` section 3 records this as a gap: "Clip
analysis is CLI-only ... there is no desktop review/acceptance surface for
generated suggestions."

This closes that gap in both directions: the app runs the pipelines, and the app
shows what they produced.

## Decisions

1. **One chained action, not per-stage buttons.** "Analyze project" runs
   proxies, then transcription, then cloud analysis, under a single staged
   progress bar. Proxies are stage zero rather than a precondition because both
   later stages read the proxy and fail per clip without one, so a chain that
   omitted them would fail every clip on a freshly ingested project.
2. **Cancellation is a first-class requirement, not a nicety.** Stage two spends
   money per clip. A run that cannot be stopped is the wrong default.
3. **Results blend in as native UI.** The AI title becomes the clip's display
   name; the transcript and observation are inspector sections beside labels and
   notes. Provenance is available but quiet — no badge-per-field treatment.
4. **The run never downloads a model.** Model installation stays explicit and
   user-initiated, preserving the existing invariant that a batch must not
   silently start a 574 MB transfer.
5. **Analysis output remains a suggestion layer.** Human notes are untouched,
   human-authored labels are untouched, and an edit to an AI-written title or
   description survives re-analysis of that clip.

## Architecture

The chain is one Tauri command over three existing `Catalog` walks:

```text
analyze_project(project_id, options)
  stage 0  generate_missing_proxies_with_progress   local, ffmpeg, ≤4 workers
  stage 1  transcribe_missing_with_progress          local, whisper, 1 worker
  stage 2  analyze_missing_with_options              cloud, OpenRouter, 4 workers
```

Each stage emits `analysis-progress` carrying `{stage, stageIndex, stageCount,
completed, total, asset, estimate, costUsd}`. The frontend reuses the merge
behavior it already applies to `proxy-progress`: a completed asset in the event
replaces the cached asset immediately, so titles and thumbnails appear as they
land rather than after the run.

No new concurrency is introduced. Each stage keeps the worker count, task key,
and `BatchEstimator` history it already has; the desktop supplies no new
estimator logic.

### Resumability

Every stage's `*_missing` walk keys on the proxy fingerprint and skips clips
that are already current, and every clip commits as it completes. Cancel, quit,
and crash therefore all leave a coherent catalog, and re-running resumes. This
also means regenerating a proxy correctly invalidates that clip's transcript and
observation, so stage zero producing a new proxy re-enqueues that clip
downstream within the same run.

### Core seams

Three changes in `rcdvlog-core`, each small and each preserving current CLI
behavior:

1. **Cancellation.** Batch progress callbacks return `ControlFlow<()>`. In the
   VLM batch this feeds the `Arc<AtomicBool>` stop flag that already exists for
   the budget cap, and the callback already runs serially on the collector
   thread, so no new synchronization is required. Workers observe the flag
   between clips, so cancel stops before the next request rather than mid-flight.
   CLI callers return `Continue`.
2. **Injected key and budget.** `analyze_missing_with_options` takes an explicit
   API key, optional budget ceiling, and worker count.
   `analyze_missing_with_progress` keeps its current signature and delegates,
   resolving from `OPENROUTER_API_KEY` / `~/.openrouter.key` and
   `RCDVLOG_VLM_MAX_COST_USD` / `RCDVLOG_VLM_WORKERS` exactly as today. The
   desktop passes the Keychain value and the ceiling from its confirm dialog.
3. **Shared model directory.** `model_dir` resolves to a shared application
   support directory, with the existing per-catalog `cache/models` path retained
   as a fallback read so current installs keep working. Without this, each
   project needs its own 574 MB copy.

### Spend accounting

Migration 15 adds `analysis_runs`:

```text
id, project_id, started_at, finished_at, status, stage_reached,
clips_attempted, clips_analyzed, cost_usd, cancelled_by_budget
```

One row per run, with `cost_usd` summed from the per-clip `cost_usd` already
recorded in `asset_analyses`. This is what makes the pre-run estimate honest:
the confirm dialog projects from the observed cost-per-clip of prior runs on
this project rather than a static guess, and reports the last run's actual spend.

### Readiness

`analysis_readiness(project_id)` returns, without touching the network: pending
counts for each stage, whether each model file is present and verifies, whether
an API key is available, and prior cost-per-clip. It is the single source for
both the confirm dialog and the disabled/blocked states, and it extends the
existing `prerequisite_status` idea rather than duplicating it.

## Desktop UI

### Clip identity

The AI title replaces the filename as the clip's display name in the queue row
and the inspector header, with the filename demoted to secondary metadata.
Clips without a title fall back to the filename, so an unanalyzed project looks
exactly as it does today. The title is editable in place; an edit is a human
edit and survives re-analysis.

### Inspector sections

Beside the existing labels, orientation, notes, and metadata sections:

- **Transcript** — timestamped segments, click to seek. `no_speech` renders as a
  quiet explicit state, never as an error, because half the corpus is silent by
  design.
- **Observation** — AI description, camera-movement chip, extracted OCR text,
  and a seek-to-best-moment control. Provider, model, prompt version, and cost
  sit in the metadata grid.
- **Unusable** — when the observation says the clip is unusable, the reason is
  shown here and the clip is badged in the queue.

### Queue

- Transcript text joins the existing filename/labels/notes filter.
- A filter toggle isolates clips the analysis flagged unusable, so triage is a
  scan rather than a per-clip scrub.

### Run control

The confirm dialog states, before spending anything: how many clips each stage
will touch, the projected stage-two cost with the basis for the projection, and
the budget ceiling for the run. It blocks with an actionable message when models
or the key are missing, linking to Settings. During a run the progress strip
shows the current stage, per-stage counts, ETA from `BatchEstimator`, running
cost, and Cancel.

### Settings

Global settings gain an analysis group: the OpenRouter key stored in the macOS
Keychain and never written to a project catalog, and a model installer that
shows each model's size and source with download progress and blake3
verification. Installation is the only path that fetches anything.

## Testing

- Core: cancellation stops before the next request; the budget ceiling still
  halts a run; readiness reports each missing prerequisite distinctly;
  migration 15 upgrades an older schema fixture; `analysis_runs` totals match
  the summed per-clip costs.
- Tauri: each new command registered and callable; the chained command reports
  stage transitions; cancel is observable.
- Frontend: `pnpm --dir apps/desktop check` and `build`, plus interaction checks
  in the Vite preview for the transcript panel, queue filter, confirm dialog,
  and progress strip at 1440×900 and the 980×680 minimum.
- Full gate: `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace`, `pnpm check`.

Browser-preview passes are not evidence for Keychain access, model download,
media processing, or persistence; those need a real `pnpm dev` smoke test.

## Out of scope

Local VLM inference, embeddings, semantic search, per-clip re-analysis from the
review pane, accept/reject workflows for individual suggestions, and any
movement of rough-cut selection into the labeling phase.
