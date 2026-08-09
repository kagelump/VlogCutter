/**
 * Proxy and thumbnail generation.
 *
 * The rule this module exists to keep: a generation command that reports work
 * changed the assets it reported. The browser preview's
 * `generateMissingProxies` returns `{ generated: 2, skipped: 5 }` and touches
 * nothing, so a later `listAssets` still shows no proxies — a test written
 * against that would pass against a lie.
 *
 * Batches are scriptable. `harness.job("generate_missing_proxies")` leaves the
 * command pending, turns each `progress` step into a real `proxy-progress`
 * event *and* the corresponding state change, and resolves only when the test
 * completes or fails it. Without a claim the batch runs to completion
 * immediately, which is what an un-scripted CUJ wants.
 */

import type { Asset, BatchEstimate, GenerationProgress, ThumbnailReport } from "../../../types";
import type { JobBinding } from "../jobs";
import { patchAsset, projectAssets, putAsset, requireAsset, type DesktopHarnessState } from "../state";
import type { CommandTable, HarnessContext } from "../registry";
import { requireString } from "../registry";

export const PROXY_REPRESENTATION = "raw_v3" as const;

/**
 * The backend's own definition of a proxy that does not need remaking
 * (`has_current_proxy` in `core/rcdvlog-core`): present *and* made by the
 * current recipe. A proxy from an older recipe is work outstanding, which is
 * why `App`'s missing-proxy count checks the representation too.
 */
function hasCurrentProxy(asset: Asset): boolean {
  return asset.proxyAvailable && asset.proxyRepresentation === PROXY_REPRESENTATION;
}

/** The proxy an asset gets. Path is derived from the id so it is stable. */
function proxyFor(state: DesktopHarnessState, assetId: string): Asset {
  const asset = requireAsset(state, assetId);
  if (!asset.originalAvailable) throw new Error(`Cannot make a proxy: the original for ${asset.filename} is offline`);
  return patchAsset(state, assetId, {
    proxyPath: `/Users/harness/Movies/proxies/${assetId}.v3.mp4`,
    proxyRepresentation: PROXY_REPRESENTATION,
    proxyAvailable: true,
  });
}

function thumbnailFor(state: DesktopHarnessState, assetId: string, quality: number): Asset {
  return patchAsset(state, assetId, {
    thumbnailPath: `/Users/harness/Movies/thumbnails/${assetId}/q${quality}.jpg`,
    thumbnailQuality: quality,
  });
}

function estimate(completed: number, total: number): BatchEstimate {
  return {
    completedItems: completed,
    totalItems: total,
    completedWorkUnits: completed,
    totalWorkUnits: total,
    workerCount: 4,
    workUnitsPerWorkerSecond: 1.4,
    elapsedSeconds: completed,
    estimatedRemainingSeconds: Math.max(0, total - completed),
  };
}

export interface GenerationStep {
  /** Which asset finished. Defaults to the next pending one. */
  assetId?: string;
  /** Mark it failed instead of generated. */
  failed?: string;
}

export interface GenerationOutcome {
  /** Assets left unfinished when the batch ends early. */
  cancelled?: boolean;
}

interface BatchPlan {
  pending: string[];
  generated: string[];
  skipped: number;
  failures: Array<{ path: string; error: string }>;
}

function runStep(
  state: DesktopHarnessState,
  plan: BatchPlan,
  step: GenerationStep,
  apply: (assetId: string) => Asset,
): GenerationProgress {
  const assetId = step.failed ?? step.assetId ?? plan.pending[0];
  if (!assetId) throw new Error("The batch has no pending items left to report");
  plan.pending = plan.pending.filter((pending) => pending !== assetId);
  if (step.failed) {
    const asset = requireAsset(state, assetId);
    plan.failures.push({ path: asset.path, error: `Generation failed for ${asset.filename}` });
    return { asset, estimate: estimate(plan.generated.length + plan.failures.length, plan.generated.length + plan.failures.length + plan.pending.length) };
  }
  const updated = apply(assetId);
  plan.generated.push(assetId);
  return {
    asset: updated,
    estimate: estimate(
      plan.generated.length + plan.failures.length,
      plan.generated.length + plan.failures.length + plan.pending.length,
    ),
  };
}

/**
 * Run a batch, scripted or not.
 *
 * The un-scripted path applies every pending item at once, which is what a test
 * that only cares about the end state wants. The scripted path hands control to
 * the test and applies items as it reports them.
 */
function runBatch<TResult>(
  context: HarnessContext,
  command: string,
  event: string,
  plan: BatchPlan,
  apply: (assetId: string) => Asset,
  report: (plan: BatchPlan, cancelled: boolean) => TResult,
): TResult | Promise<unknown> {
  const controller = context.jobs.claimed(command);
  if (!controller) {
    for (const assetId of [...plan.pending]) {
      runStep(context.state, plan, { assetId }, apply);
    }
    return report(plan, false);
  }
  const binding: JobBinding<GenerationStep, GenerationOutcome, TResult> = {
    command,
    args: {},
    event,
    progress: (step) => runStep(context.state, plan, step, apply),
    result: (outcome) => report(plan, Boolean(outcome?.cancelled) || controller.isCancelled),
  };
  return controller.bind(binding);
}

export const mediaCommands: CommandTable = {
  generate_proxy: (args, { state }) => proxyFor(state, requireString(args, "assetId")),

  generate_missing_proxies: (args, context) => {
    const projectId = requireString(args, "projectId");
    const assets = projectAssets(context.state, projectId);
    const plan: BatchPlan = {
      pending: assets.filter((asset) => !hasCurrentProxy(asset) && asset.originalAvailable).map((asset) => asset.id),
      generated: [],
      skipped: assets.filter(hasCurrentProxy).length,
      failures: [],
    };
    return runBatch(context, "generate_missing_proxies", "proxy-progress", plan, (assetId) => proxyFor(context.state, assetId), (finished, cancelled) => ({
      generated: finished.generated.length,
      skipped: finished.skipped,
      cancelled,
      failures: finished.failures,
    }));
  },

  generate_thumbnail: (args, { state }) => {
    const quality = typeof args.quality === "number" ? args.quality : 1;
    return thumbnailFor(state, requireString(args, "assetId"), quality);
  },

  generate_missing_thumbnails: (args, context) => {
    const projectId = requireString(args, "projectId");
    const quality = typeof args.quality === "number" ? args.quality : 1;
    const assets = projectAssets(context.state, projectId);
    const plan: BatchPlan = {
      pending: assets.filter((asset) => asset.thumbnailQuality < quality).map((asset) => asset.id),
      generated: [],
      skipped: assets.filter((asset) => asset.thumbnailQuality >= quality).length,
      failures: [],
    };
    return runBatch(
      context,
      "generate_missing_thumbnails",
      "thumbnail-progress",
      plan,
      (assetId) => thumbnailFor(context.state, assetId, quality),
      (finished): ThumbnailReport => ({
        quality,
        generated: finished.generated.length,
        skipped: finished.skipped,
        failures: finished.failures,
      }),
    );
  },

  /**
   * Point the app at a folder to find originals it has lost track of.
   *
   * The scenario says which clips are actually in that folder
   * (`relinkableAt`); everything else stays missing, because a relink that
   * repairs an asset the folder does not contain is the bug this flow exists to
   * avoid.
   */
  relink_folder: (args, { state }) => {
    const projectId = requireString(args, "projectId");
    const path = requireString(args, "path");
    const assets = projectAssets(state, projectId);
    const found = new Set(state.relinkable[path] ?? []);
    const missing = assets.filter((asset) => !asset.originalAvailable);
    const repaired = missing.filter((asset) => found.has(asset.id));
    for (const asset of repaired) {
      patchAsset(state, asset.id, { originalAvailable: true, available: true });
    }
    return {
      scanned: found.size,
      repaired: repaired.length,
      alreadyOnline: assets.filter((asset) => asset.originalAvailable && found.has(asset.id)).length,
      unrecognized: [...found].filter((assetId) => !state.assets[assetId]).length,
      stillMissing: missing.length - repaired.length,
      failures: [],
    };
  },

  ingest_folder: (args, { state }) => {
    const projectId = requireString(args, "projectId");
    const path = requireString(args, "path");
    const additions = (state.imports.additions[path] ?? []).filter((asset) => asset.projectId === projectId);
    const fresh = additions.filter((asset) => !state.assets[asset.id]);
    for (const asset of fresh) putAsset(state, asset);
    return {
      scanned: additions.length,
      imported: fresh.length,
      duplicates: additions.length - fresh.length,
      relinked: 0,
      failures: [],
    };
  },
};
