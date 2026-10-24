/**
 * Clip analysis reads and prerequisites.
 *
 * Readiness is derived from canonical state — clips, proxies, transcripts, and
 * observations — rather than declared, so installing a model or completing a run
 * changes what the gate says without a test restating it.
 *
 * Run control (`analyze_project`, `cancel_analysis`) is scripted and lives with
 * the analysis CUJs; see H01-T004.
 */

import type {
  AnalysisProgress,
  AnalysisReadiness,
  AnalysisRunReport,
  AnalysisStage,
  AssetAnalysisIndexEntry,
  ChronologySuggestionResult,
  ModelStatus,
} from "../../../types";
import { buildAnalysis, buildTranscript, type AnalysisSpec, type TranscriptSpec } from "../builders";
import type { JobBinding } from "../jobs";
import { projectAssets, requireAsset, type DesktopHarnessState } from "../state";
import type { CommandTable, HarnessContext } from "../registry";
import { requireBoolean, requireString } from "../registry";

const STAGES: AnalysisStage[] = ["proxies", "transcripts", "analysis"];

/**
 * One step of a run, as the test describes it.
 *
 * A step names the stage and the clip it reached. Transcripts and analyses are
 * written into canonical state as they are reported, so the evidence shelf, the
 * queue index, and the unusable filter observe them during the run rather than
 * after it — which is what the production progress handler is for.
 */
export interface AnalysisStep {
  stage: AnalysisStage;
  assetId: string;
  transcript?: TranscriptSpec;
  analysis?: AnalysisSpec;
  /** Completed count to report; defaults to how many steps this stage has had. */
  completed?: number;
  total?: number;
}

export interface AnalysisOutcome {
  status?: "completed" | "cancelled" | "failed";
  budgetExhausted?: boolean;
  usageComplete?: boolean;
  failures?: Array<{ path: string; error: string }>;
}

interface RunTally {
  projectId: string;
  perStage: Record<AnalysisStage, number>;
  costUsd: number;
  stageReached: AnalysisStage;
  cancelled: boolean;
}

function applyStep(state: DesktopHarnessState, tally: RunTally, step: AnalysisStep): AnalysisProgress {
  const asset = requireAsset(state, step.assetId);
  tally.perStage[step.stage] += 1;
  tally.stageReached = step.stage;

  if (step.stage === "transcripts") {
    state.transcripts[asset.id] = buildTranscript(asset.id, step.transcript ?? {});
  }
  if (step.stage === "analysis") {
    state.analyses[asset.id] = buildAnalysis(asset.id, step.analysis ?? {});
    // Cost accumulates from the work actually reported. A completion may never
    // invent a total the steps did not reach.
    tally.costUsd += state.analyses[asset.id].analysis.costUsd;
  }

  const active = projectAssets(state, tally.projectId).filter((candidate) => !candidate.removed);
  const total = step.total ?? active.length;
  const completed = step.completed ?? tally.perStage[step.stage];
  return {
    stage: step.stage,
    stageIndex: STAGES.indexOf(step.stage),
    stageCount: STAGES.length,
    asset,
    estimate: {
      completedItems: completed,
      totalItems: total,
      completedWorkUnits: completed,
      totalWorkUnits: total,
      workerCount: step.stage === "transcripts" ? 1 : 4,
      workUnitsPerWorkerSecond: 1.4,
      elapsedSeconds: completed,
      estimatedRemainingSeconds: Math.max(0, total - completed),
    },
    costUsd: tally.costUsd,
  };
}

function runReport(state: DesktopHarnessState, tally: RunTally, outcome: AnalysisOutcome, clock: { iso(): string }): AnalysisRunReport {
  const status = outcome.status ?? (tally.cancelled ? "cancelled" : "completed");
  const failures = outcome.failures ?? [];
  const report: AnalysisRunReport = {
    runId: `run_harness_${state.analysisRuns.length + 1}`,
    status,
    stageReached: tally.stageReached,
    proxies: { generated: tally.perStage.proxies, skipped: 0, cancelled: status === "cancelled", failures: [] },
    transcripts: {
      transcribed: tally.perStage.transcripts,
      noAudio: 0,
      failed: 0,
      skipped: 0,
      cancelled: status === "cancelled",
      failures: [],
    },
    analysis: {
      analyzed: tally.perStage.analysis,
      skipped: 0,
      rejected: 0,
      fallbackServed: 0,
      costUsd: tally.costUsd,
      usageComplete: outcome.usageComplete ?? true,
      cancelled: status === "cancelled",
      budgetExhausted: Boolean(outcome.budgetExhausted),
      failures,
    },
    costUsd: tally.costUsd,
  };
  state.analysisRuns.push({
    id: report.runId,
    projectId: tally.projectId,
    startedAt: clock.iso(),
    finishedAt: clock.iso(),
    status,
    stageReached: report.stageReached,
    accountingContract: "",
    policyId: "",
    proxiesGenerated: report.proxies.generated,
    clipsTranscribed: report.transcripts.transcribed,
    clipsAnalyzed: report.analysis.analyzed,
    costUsd: report.costUsd,
    budgetExhausted: report.analysis.budgetExhausted,
    error: "",
  });
  return report;
}

/**
 * An un-scripted run analyses every clip that needs it, which is what a CUJ
 * about the *result* wants. A scripted run hands the stages to the test.
 */
function startRun(context: HarnessContext, projectId: string): AnalysisRunReport | Promise<unknown> {
  const tally: RunTally = {
    projectId,
    perStage: { proxies: 0, transcripts: 0, analysis: 0 },
    costUsd: 0,
    stageReached: "proxies",
    cancelled: false,
  };
  const controller = context.jobs.claimed("analyze_project");
  if (!controller) {
    const active = projectAssets(context.state, projectId).filter((asset) => !asset.removed);
    for (const asset of active) {
      if (!context.state.transcripts[asset.id]) applyStep(context.state, tally, { stage: "transcripts", assetId: asset.id });
    }
    for (const asset of active) {
      if (!context.state.analyses[asset.id]) applyStep(context.state, tally, { stage: "analysis", assetId: asset.id });
    }
    return runReport(context.state, tally, {}, context.clock);
  }

  const binding: JobBinding<AnalysisStep, AnalysisOutcome, AnalysisRunReport> = {
    command: "analyze_project",
    args: { projectId },
    event: "analysis-progress",
    progress: (step) => applyStep(context.state, tally, step),
    onCancel: () => {
      tally.cancelled = true;
    },
    result: (outcome) => {
      tally.cancelled = tally.cancelled || controller.isCancelled;
      return runReport(context.state, tally, outcome, context.clock);
    },
  };
  return controller.bind(binding);
}

export const analysisCommands: CommandTable = {
  analysis_readiness: (args, { state }): AnalysisReadiness => {
    const projectId = requireString(args, "projectId");
    const active = projectAssets(state, projectId).filter((asset) => !asset.removed);
    const missingAnalyses = active.filter((asset) => !state.analyses[asset.id]).length;
    return {
      projectId,
      totalClips: active.length,
      reachableOriginals: active.filter((asset) => asset.originalAvailable).length,
      unverifiedVideoClips: 0,
      quarantinedVideoClips: 0,
      unavailableVideoClips: 0,
      missingProxies: active.filter((asset) => !asset.proxyAvailable).length,
      missingTranscripts: active.filter((asset) => !state.transcripts[asset.id]).length,
      missingAnalyses,
      models: state.models,
      modelsReady: state.models.every((model) => model.installed),
      apiKeyPresent: state.openRouterKeyPresent,
      estimatedCostUsd: missingAnalyses * state.costPerClipUsd,
      costBasis: "observed",
      observedCostPerClipUsd: state.costPerClipUsd,
      lastRun: state.analysisRuns.at(-1) ?? null,
    };
  },

  project_analysis_index: (args, { state }): AssetAnalysisIndexEntry[] =>
    projectAssets(state, requireString(args, "projectId")).map((asset) => {
      const transcript = state.transcripts[asset.id];
      const analysis = state.analyses[asset.id];
      return {
        assetId: asset.id,
        transcriptStatus: transcript?.status ?? null,
        transcriptText: (transcript?.segments ?? []).map((segment) => segment.text).join(" "),
        analyzed: Boolean(analysis),
        usable: analysis?.analysis.usable ?? true,
        unusableReason: analysis?.analysis.unusableReason ?? "",
      };
    }),

  asset_transcript: (args, { state }) => state.transcripts[requireString(args, "assetId")] ?? null,

  asset_analysis: (args, { state }) => state.analyses[requireString(args, "assetId")] ?? null,

  analysis_suggest_chronology: (args, { state }): ChronologySuggestionResult => {
    const projectId = requireString(args, "projectId");
    const assetIds = args.assetIds;
    if (!Array.isArray(assetIds) || assetIds.some((assetId) => typeof assetId !== "string")) {
      throw new Error("Harness command argument \"assetIds\" must be an array of strings");
    }
    if (assetIds.length < 2 || assetIds.length > 8 || new Set(assetIds).size !== assetIds.length) {
      throw new Error("Chronology evidence requires 2–8 distinct clips");
    }
    for (const assetId of assetIds) {
      const asset = requireAsset(state, assetId);
      if (asset.projectId !== projectId) throw new Error(`Asset ${assetId} does not belong to project ${projectId}`);
    }
    const primaryModel = requireString(args, "primaryModel");
    const allowedModels = args.allowedModels;
    if (!Array.isArray(allowedModels) || allowedModels.some((model) => typeof model !== "string")) {
      throw new Error("Harness command argument \"allowedModels\" must be an array of strings");
    }
    if (allowedModels.length > 0 && !allowedModels.includes(primaryModel)) {
      throw new Error("The primary model must be included in allowedModels");
    }
    requireBoolean(args, "allowFallback");

    // This is a local deterministic harness response, not provider behavior.
    // It cites two synthetic proxy-frame IDs per clip and makes no chronology
    // changes; tests can exercise the DTO and command contract without a paid call.
    const frameIds = assetIds.flatMap((assetId) => [`${assetId}:proxy-first`, `${assetId}:proxy-last`]);
    return {
      suggestion: { frameIds, matches: [] },
      review: { missingFrameIds: [], duplicateFrameIds: [], unknownFrameIds: [], semanticInconsistencies: [] },
      provenance: {
        model: primaryModel,
        attempts: 1,
        attemptedModels: [primaryModel],
        usageComplete: true,
        costUsd: 0,
        promptTokens: 0,
        completionTokens: 0,
        latencyMs: 0,
      },
      cached: false,
    };
  },

  analysis_runs: (args, { state }) => {
    const projectId = requireString(args, "projectId");
    const limit = typeof args.limit === "number" ? args.limit : 5;
    return state.analysisRuns
      .filter((run) => run.projectId === projectId)
      .slice(-limit)
      .reverse();
  },

  model_status: (_args, { state }): ModelStatus[] => state.models,

  install_analysis_models: (_args, { state }) => {
    state.models = state.models.map((model) => ({ ...model, installed: true }));
    return state.models;
  },

  openrouter_key_present: (_args, { state }) => state.openRouterKeyPresent,

  set_openrouter_key: (args, { state }) => {
    const key = requireString(args, "key").trim();
    state.openRouterKey = key;
    state.openRouterKeyPresent = key.length > 0;
    return state.openRouterKeyPresent;
  },

  analyze_project: (args, context) => startRun(context, requireString(args, "projectId")),

  /**
   * Cancellation is a race the test owns. The command itself only marks the
   * run; the pending `analyze_project` resolves with what it had actually
   * reached, which is the only way "cancelled but claims to have finished"
   * becomes a test rather than a hope.
   */
  cancel_analysis: (_args, { jobs }) => {
    jobs.claimed("analyze_project")?.requestCancel();
    return null;
  },
};
