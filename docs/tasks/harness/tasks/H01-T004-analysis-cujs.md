# H01-T004: Analysis CUJs

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "H01-T004",
  "epic_id": "H01",
  "title": "Analysis CUJs",
  "slug": "analysis-cujs",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["H01-T001"],
  "principal_invariant": "The analysis readiness gate, run progress, cancellation, and result surfacing are driven entirely by test-scripted events against canonical state, with no wall-clock delay and no completion the test did not grant.",
  "central_falsification": "Cancel a run after partial progress and prove the UI cannot be made to show a completed run, a full clip count, or a cost the run never incurred.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "analysis run control -> scripted progress, outcome, and stored results"
  ],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/harness/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/harness/ARCHITECTURE.md", "docs/superpowers/specs/2026-08-04-desktop-analysis-integration-design.md", "docs/batch-estimation.md"],
    "independent_tester": ["docs/superpowers/specs/2026-08-03-stt-vlm-clip-analysis-design.md", "docs/integration-testing.md"],
    "reviewer": ["docs/tasks/harness/ARCHITECTURE.md", "AGENTS.md"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/harness/tasks/H01-T004-analysis-cujs.md"],
    "implementer": ["apps/desktop/src/test/harness/commands/analysis.ts", "apps/desktop/src/test/cuj/analysis.cuj.test.tsx", "apps/desktop/src/test/harness/scenarios/analysisReady.ts"],
    "independent_tester": ["apps/desktop/src/__independent__/analysis-run.independent.test.tsx"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "forbidden_paths": ["core/**", "apps/desktop/src-tauri/**", "apps/web/**", "apps/server/**", "apps/cli/**"],
  "verification_commands": [
    "pnpm --dir apps/desktop test",
    "pnpm --dir apps/desktop check"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/harness/EPIC.md",
      "regex": "\\| \\[H01-T004\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Cover the analysis workflow end to end without a model download, an API key, a
network call, or a sleep. CUJ 6 and 7 from the design brief.

## Principal invariant

Readiness gating, model installation, key presence, run progress, cancellation,
and result surfacing all move only when the test moves them, and a run's reported
outcome matches the transcripts, analyses, and costs the harness actually holds.

## Inputs and outputs

Inputs: harness from H01-T001; `analysis_readiness`, `analyze_project`,
`cancel_analysis`, `analysis_runs`, `project_analysis_index`, `asset_transcript`,
`asset_analysis`, `model_status`, `install_analysis_models`,
`set_openrouter_key`, `openrouter_key_present`; the `analysis-progress` and
`model-install-progress` events.

Outputs: an analysis command module, an `analysisReady` scenario, and CUJs for
the readiness-to-completion path and the cancellation path.

## Frozen decisions

- `analyze_project` returns a pending promise; the test emits stage progress and
  then completes, cancels, or fails it.
- Cancellation is a race the test controls: `cancel_analysis` marks the run, and
  the pending `analyze_project` resolves with `status: "cancelled"` and the
  counts actually reached.
- Completing a run writes transcripts and analyses into canonical state, so the
  queue index, evidence shelf, and unusable filter observe them afterwards.
- Cost is accumulated from the emitted progress, never invented at completion.
- Model installation flips `installed` in state; `install_analysis_models` may
  emit `model-install-progress` first.
- Nothing here is evidence for Keychain storage, whisper.cpp, or OpenRouter.

## Owned paths

The implementer owns the analysis command module, the scenario, and the CUJ
file. The independent tester owns one file under `__independent__/`.

## Required interface

```ts
const run = harness.job("analyze_project");
await run.progress({ stage: "transcripts", completed: 5, total: 20 });
await run.complete({ analyzed: 20, costUsd: 0.0032 });
await run.cancelled({ stageReached: "transcripts", analyzed: 0 });
harness.state.models;
harness.state.openRouterKeyPresent;
```

## Acceptance oracle

1. With a model missing, the analyze action is gated and names what is missing;
   installing the model clears that half of the gate.
2. With no key present, the gate names the key; setting one clears it, and
   `openRouterKeyPresent` reflects state.
3. Readiness shows clip counts and an estimated cost derived from state, not
   from a constant in the test.
4. Starting a run renders stage progress for each emitted event, in order, with
   no timer advance.
5. Completing a run surfaces the resulting analysis on the affected clips: the
   evidence shelf shows it, the queue index marks it analyzed, and the
   unusable-only filter can find an unusable one.
6. Cancelling mid-run shows a cancelled outcome; no completed count, no cost
   beyond what was emitted, and no analysis written for unreached clips.
7. Re-running after a cancellation completes normally.
8. The run history reflects both runs in deterministic order.

## Independent test charter

Try to manufacture a false completion: cancel between stages, cancel after the
last progress but before completion, fail mid-stage, and start a second run while
one is pending. Assert what the user sees, and check state after each.

## Review charter

Risk domain: optimistic UI that outruns the backend. Ask: can the UI show a
result the harness never produced? Is any cost or count hardcoded in a test
expectation rather than derived? Does any test advance real time? Severity: a
false-completion path is blocking.

## Non-goals

- Model correctness, prompt behavior, or provider integration.
- Batch-estimation arithmetic, which has its own Rust tests.

## Stop conditions

Stop and escalate when a cancellation cannot be represented without inventing a
backend rule, or when readiness cannot be derived from canonical state.

## Commands

```sh
pnpm --dir apps/desktop test
pnpm --dir apps/desktop check
```

## Handoff

Report the scripted job API as implemented, the stages covered, the cancellation
semantics chosen, and anything about analysis the fast tier cannot observe.
