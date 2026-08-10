/**
 * Preview-only animation of the harness's scriptable jobs.
 *
 * A CUJ drives a long operation step by step so cancellation is a test rather
 * than a hope. Nobody is driving the preview, so this stands in for the test:
 * it claims the same jobs through the same `harness.job(...)` door and releases
 * their steps on a real timer, which is what makes progress bars, elapsed time,
 * and the cancel button worth looking at.
 *
 * Everything non-deterministic about the preview lives in this file. The harness
 * never loads it: the only importer is `installPreviewBackend`, which `main.tsx`
 * reaches through a dynamic `import()` behind the `!inTauri()` check.
 *
 * No step here reports work the shared handlers do not perform. The analysis
 * driver skips the proxy stage for exactly that reason — the analysis handler
 * models transcripts and observations, and reporting proxy work it did not do is
 * the lie this whole harness exists to stop telling.
 */

import type { DesktopHarness } from "../test/harness/DesktopHarness";
import type { JobController } from "../test/harness/jobs";
import type { AnalysisOutcome, AnalysisStep } from "../test/harness/commands/analysis";
import type { GenerationOutcome, GenerationStep } from "../test/harness/commands/media";
import type { PreviewStep } from "../test/harness/commands/editPlan";
import { PROXY_REPRESENTATION } from "../test/harness/commands/media";
import { projectAssets } from "../test/harness/state";
import type { Asset } from "../types";
import type { PreviewResult } from "../chronologyTypes";

/** Slow enough to watch, quick enough to sit through. */
const STEP_MS = 420;

const DEMO_SPEECH = [
  "次は小樽、小樽です。",
  "It is properly cold out here this morning.",
  "お出口は右側です。",
  "That's the market at the end of the street.",
];

const DEMO_LABELS = [
  ["street", "morning"],
  ["market", "food"],
  ["snow", "wide"],
  ["portrait", "interior"],
];

function sleep(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function currentProjectId(harness: DesktopHarness): string {
  return harness.state.settings.lastProjectId ?? harness.state.projects[0]?.id ?? "";
}

function activeAssets(harness: DesktopHarness): Asset[] {
  return projectAssets(harness.state, currentProjectId(harness)).filter((asset) => !asset.removed);
}

/**
 * Serve one command for as long as the preview is open: wait for the app to
 * issue it, drive it, settle it, and go back to waiting.
 */
async function serve<TStep, TOutcome>(
  harness: DesktopHarness,
  command: string,
  run: (controller: JobController<TStep, TOutcome>) => Promise<void>,
): Promise<never> {
  for (;;) {
    const controller = harness.job<TStep, TOutcome>(command);
    await controller.started();
    try {
      await run(controller);
    } catch (reason) {
      // A driver that miscounts its own steps must not wedge the preview: the
      // command still settles below, and the app sees an ordinary completion.
      console.warn(`[preview] ${command} progress stopped early:`, reason);
    }
    if (!controller.isSettled) {
      if (controller.isCancelled) await controller.cancel();
      else await controller.complete();
    }
  }
}

/** Transcribe, then observe, every clip that is missing one — clip by clip. */
async function runAnalysis(
  harness: DesktopHarness,
  controller: JobController<AnalysisStep, AnalysisOutcome>,
): Promise<void> {
  const active = activeAssets(harness);
  const needTranscript = active.filter((asset) => !harness.state.transcripts[asset.id]);
  const needAnalysis = active.filter((asset) => !harness.state.analyses[asset.id]);

  const steps: AnalysisStep[] = [
    ...needTranscript.map((asset, index): AnalysisStep => ({
      stage: "transcripts",
      assetId: asset.id,
      total: needTranscript.length,
      transcript: { text: DEMO_SPEECH[index % DEMO_SPEECH.length] },
    })),
    ...needAnalysis.map((asset, index): AnalysisStep => ({
      stage: "analysis",
      assetId: asset.id,
      total: needAnalysis.length,
      analysis: { aiLabels: DEMO_LABELS[index % DEMO_LABELS.length] },
    })),
  ];

  for (const step of steps) {
    await sleep(STEP_MS);
    if (controller.isCancelled) return;
    await controller.progress(step);
  }
}

/** How many clips a proxy batch has left to make, by the backend's definition. */
function pendingProxies(harness: DesktopHarness): number {
  return projectAssets(harness.state, currentProjectId(harness)).filter(
    (asset) =>
      asset.originalAvailable &&
      !(asset.proxyAvailable && asset.proxyRepresentation === PROXY_REPRESENTATION),
  ).length;
}

async function runBatch(
  controller: JobController<GenerationStep, GenerationOutcome>,
  items: number,
): Promise<void> {
  for (let item = 0; item < items; item += 1) {
    await sleep(STEP_MS);
    if (controller.isCancelled) return;
    await controller.progress({});
  }
}

/** The clips a run would copy: the ones on the card that the project lacks. */
function pendingImports(harness: DesktopHarness): number {
  const sourcePath = harness.state.imports.lastRunSourcePath;
  if (!sourcePath) return 0;
  return (harness.state.imports.additions[sourcePath] ?? []).filter(
    (asset) => !harness.state.assets[asset.id],
  ).length;
}

export function startPreviewJobs(harness: DesktopHarness): void {
  void serve<AnalysisStep, AnalysisOutcome>(harness, "analyze_project", (controller) =>
    runAnalysis(harness, controller),
  );

  void serve<GenerationStep, GenerationOutcome>(harness, "generate_missing_proxies", (controller) =>
    runBatch(controller, pendingProxies(harness)),
  );

  void serve<undefined, { cancelled?: boolean }>(harness, "run_smart_import", async (controller) => {
    const items = pendingImports(harness);
    for (let item = 0; item < items; item += 1) {
      await sleep(STEP_MS);
      if (controller.isCancelled) return;
      await controller.progress(undefined);
    }
  });

  void serve<PreviewStep, { failures?: PreviewResult["failures"] }>(harness, "render_edit_plan_preview", async (controller) => {
    const clips = harness.state.editPlans.active?.operations.length ?? 0;
    for (let position = 0; position < clips; position += 1) {
      await sleep(STEP_MS);
      if (controller.isCancelled) return;
      await controller.progress({ position });
    }
  });
}
