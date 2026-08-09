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
  AnalysisReadiness,
  AssetAnalysisIndexEntry,
  ModelStatus,
} from "../../../types";
import { projectAssets } from "../state";
import type { CommandTable } from "../registry";
import { requireString } from "../registry";

export const analysisCommands: CommandTable = {
  analysis_readiness: (args, { state }): AnalysisReadiness => {
    const projectId = requireString(args, "projectId");
    const active = projectAssets(state, projectId).filter((asset) => !asset.removed);
    const missingAnalyses = active.filter((asset) => !state.analyses[asset.id]).length;
    return {
      projectId,
      totalClips: active.length,
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
};
