/**
 * The canonical fake backend state.
 *
 * One object, read and written by every command handler. The load-bearing rule
 * of this harness lives here:
 *
 *   A successful mutating command must change this state such that every later
 *   read observes it.
 *
 * The browser-preview mock in `api.ts` reports two generated proxies and changes
 * no asset, so a later `listAssets` still shows none. A test written against
 * that would pass against a lie. Nothing in this harness may report work it did
 * not do.
 */

import type {
  AnalysisRunRecord,
  ArchiveRoot,
  Asset,
  AssetAnalysisView,
  CameraMapping,
  CardCandidate,
  ClipTranscript,
  DesktopSettings,
  ImportPreflight,
  ImportSessionDetail,
  MediaOrigin,
  ModelStatus,
  Project,
  ServerVersionInfo,
  SyncStatus,
} from "../../types";
import type {
  CaptureSequenceMergePreview,
  CaptureSequenceReassignmentPreview,
  CaptureSequenceSplitPreview,
  ChronologyExplanation,
  ChronologyImpact,
  ChronologyOverview,
  ClockEpochClearPreview,
  ClockEpochImpactPreview,
  Contradiction,
  Diagnostic,
  EditPlanRevision,
  OtioExportResult,
  QueuePosition,
  ValidationReport,
  VisualPlacementPreview,
} from "../../chronologyTypes";
import { HarnessClock } from "./clock";

/**
 * Chronology is declared, never computed.
 *
 * `rcdvlog-core` owns the solver. A second solver in TypeScript could disagree
 * with it and would then be a machine for producing false confidence, so a
 * scenario states the overview the backend returns, and — keyed by a named edit
 * — the overview it returns afterwards.
 */
export interface ChronologyScenarioState {
  overview: ChronologyOverview;
  /** Overview to switch to once the named edit is applied. */
  outcomesByEdit: Record<string, ChronologyOverview>;
  /** Contradictions a preview returns, keyed by the same edit name. */
  previewsByEdit: Record<string, Contradiction[]>;
  /** Exact solver-declared preview of a candidate epoch, keyed by capture sequence. */
  impactsByEdit: Record<string, ClockEpochImpactPreview>;
  /** Exact solver-declared impact of removing an epoch, keyed by epoch id. */
  clearEpochImpacts: Record<string, ClockEpochClearPreview>;
  /** Exact solver-declared response for a visual placement request. */
  visualPlacementPreviews: Record<string, VisualPlacementPreview>;
  /** Exact solver-declared response for a card reassignment request. */
  reassignmentPreviews: Record<string, CaptureSequenceReassignmentPreview>;
  /** Overview returned after a declared card reassignment is applied. */
  reassignmentOutcomes: Record<string, ChronologyOverview>;
  /** Exact solver-declared response for a card split request. */
  sequenceSplitPreviews: Record<string, CaptureSequenceSplitPreview>;
  /** Overview returned after an exact split preview is applied, keyed by facts token. */
  sequenceSplitOutcomes: Record<string, ChronologyOverview>;
  /** Exact solver-declared response for a whole-card merge request. */
  sequenceMergePreviews: Record<string, CaptureSequenceMergePreview>;
  /** Overview returned after an exact merge preview is applied, keyed by facts token. */
  sequenceMergeOutcomes: Record<string, ChronologyOverview>;
  explanations: Record<string, ChronologyExplanation>;
  /** Queue positions captured by the app, so restore can return the same clip. */
  capturedPositions: QueuePosition[];
  /** What `restore_queue_position` resolves to, when the scenario overrides it. */
  restoreTo: string | null;
}

export interface EditPlanScenarioState {
  active: EditPlanRevision | null;
  revisions: EditPlanRevision[];
  validation: Record<string, ValidationReport>;
  exportResult: OtioExportResult | null;
  /** Set when the scenario wants the next write to hit a revision conflict. */
  conflictMessage: string | null;
}

export interface PairingScenarioState {
  serverInfo: ServerVersionInfo;
  /** Pairing codes the fake server accepts. Anything else is rejected. */
  acceptedCodes: string[];
  baselineDigest: string;
}

export interface ImportScenarioState {
  /** Preflight results keyed by source path, declared by the scenario. */
  preflights: Record<string, ImportPreflight>;
  /** Clips a run will add, keyed by source path. */
  additions: Record<string, Asset[]>;
  lastRunSourcePath: string | null;
  /** Durable import history, keyed by its deterministic harness id. */
  sessions: Record<string, ImportSessionDetail>;
  /**
   * Durable per-project camera-identity mappings (#71), keyed by project id.
   * This is the harness's stand-in for the real `camera_identity_mappings`
   * SQLite table — it lives on `harness.state`, not in any component, so it
   * survives an `app.remount()` the same way the real table survives an app
   * restart within one CUJ test.
   */
  cameraMappings: Record<string, CameraMapping[]>;
}

export interface DesktopHarnessState {
  settings: DesktopSettings;
  prerequisites: Array<{ name: string; path: string; available: boolean }>;
  catalogPath: string;

  projects: Project[];
  /** Insertion order for `list_assets`; the app does its own ordering on top. */
  assetOrder: string[];
  assets: Record<string, Asset>;

  transcripts: Record<string, ClipTranscript>;
  analyses: Record<string, AssetAnalysisView>;
  analysisRuns: AnalysisRunRecord[];
  models: ModelStatus[];
  openRouterKeyPresent: boolean;
  openRouterKey: string;
  /** Per-clip cost the fake charges, so a run's total is derived and not invented. */
  costPerClipUsd: number;

  chronology: ChronologyScenarioState;
  diagnostics: Diagnostic[];
  acknowledgedDiagnostics: string[];
  editPlans: EditPlanScenarioState;

  archiveRoots: ArchiveRoot[];
  cards: CardCandidate[];
  imports: ImportScenarioState;
  /** Asset ids a relink at a given folder would actually find. */
  relinkable: Record<string, string[]>;

  pairing: PairingScenarioState;
  sync: Record<string, SyncStatus>;

  mediaOrigin: MediaOrigin | null;
  /** Paths a native dialog would return, consumed in order. */
  dialogResponses: Array<string | null>;
}

export const EMPTY_CHRONOLOGY_OVERVIEW: ChronologyOverview = {
  sequences: [],
  epochs: [],
  exceptions: [],
  phases: [],
  constraints: [],
  contradictions: [],
  assets: [],
  proposedSplits: [],
  declinedSplits: [],
  unplacedAssets: 0,
};

export function createInitialState(clock: HarnessClock): DesktopHarnessState {
  return {
    settings: {
      projectsRoot: "/Users/harness/Movies/VideoProjects",
      nodeId: "node_desktop_harness",
      autoplayMode: "proxyAvailable",
      lastCatalogPath: null,
      lastProjectId: null,
      lastAssetId: null,
      lastContextUpdatedAt: null,
      makeProxiesOnImport: true,
      lastImportArchiveRootId: null,
      importArchiveRootIdsByCardSession: {},
      pairedServerNodeId: null,
      pairedServerEndpoint: null,
      pairedServerLabel: null,
      pairedAt: null,
      pairedServerEndpoints: null,
      mediaOrigin: null,
    },
    prerequisites: ["ffmpeg", "ffprobe", "exiftool"].map((name) => ({
      name,
      path: `/opt/homebrew/bin/${name}`,
      available: true,
    })),
    catalogPath: "/Users/harness/Movies/VideoProjects/catalog.sqlite3",

    projects: [],
    assetOrder: [],
    assets: {},

    transcripts: {},
    analyses: {},
    analysisRuns: [],
    models: [
      {
        name: "ggml-large-v3-turbo-q5_0.bin",
        installed: true,
        bytes: 574_000_000,
        url: "https://huggingface.co/ggerganov/whisper.cpp",
        directory: "/Users/harness/Library/Application Support/com.rcd.vlogcutter/models",
      },
      {
        name: "ggml-silero-v5.1.2.bin",
        installed: true,
        bytes: 885_000,
        url: "https://huggingface.co/ggml-org/whisper-vad",
        directory: "/Users/harness/Library/Application Support/com.rcd.vlogcutter/models",
      },
    ],
    openRouterKeyPresent: true,
    openRouterKey: "harness-key",
    costPerClipUsd: 0.0001084,

    chronology: {
      overview: EMPTY_CHRONOLOGY_OVERVIEW,
      outcomesByEdit: {},
      previewsByEdit: {},
      impactsByEdit: {},
      clearEpochImpacts: {},
      visualPlacementPreviews: {},
      reassignmentPreviews: {},
      reassignmentOutcomes: {},
      sequenceSplitPreviews: {},
      sequenceSplitOutcomes: {},
      sequenceMergePreviews: {},
      sequenceMergeOutcomes: {},
      explanations: {},
      capturedPositions: [],
      restoreTo: null,
    },
    diagnostics: [],
    acknowledgedDiagnostics: [],
    editPlans: {
      active: null,
      revisions: [],
      validation: {},
      exportResult: null,
      conflictMessage: null,
    },

    archiveRoots: [],
    cards: [],
    imports: { preflights: {}, additions: {}, lastRunSourcePath: null, sessions: {}, cameraMappings: {} },
    relinkable: {},

    pairing: {
      serverInfo: {
        nodeId: "node_server_harness",
        applicationVersion: "0.1.0",
        protocolMajor: 1,
        protocolMinor: 0,
      },
      acceptedCodes: ["123456"],
      baselineDigest: "digest_harness_001",
    },
    sync: {},

    mediaOrigin: {
      baseUrl: "http://127.0.0.1:54321",
      token: "harness-media-token",
      debugUrl: null,
    },
    dialogResponses: [],
  };
}

/**
 * The assets of one project, in the order the catalog returns them.
 *
 * `orderAssets` in the app filters but never sorts — the queue's order *is*
 * this read's order — so the fake reproduces the catalog's clause exactly:
 *
 *   ORDER BY best_time IS NULL, best_time, filename, id
 *
 * (`core/rcdvlog-core/src/lib.rs`, `list_assets`). Returning insertion order
 * instead would quietly give every CUJ a different queue than the real app.
 */
export function projectAssets(state: DesktopHarnessState, projectId: string): Asset[] {
  return state.assetOrder
    .map((id) => state.assets[id])
    .filter((asset): asset is Asset => Boolean(asset) && asset.projectId === projectId)
    .sort((left, right) => {
      const leftMissing = left.bestTime === null ? 1 : 0;
      const rightMissing = right.bestTime === null ? 1 : 0;
      if (leftMissing !== rightMissing) return leftMissing - rightMissing;
      const byTime = (left.bestTime ?? "").localeCompare(right.bestTime ?? "");
      if (byTime !== 0) return byTime;
      const byFilename = left.filename.localeCompare(right.filename);
      return byFilename !== 0 ? byFilename : left.id.localeCompare(right.id);
    });
}

/**
 * Project counts are derived on every read rather than stored, so a mutation
 * cannot leave a summary disagreeing with the assets it summarises.
 */
export function projectSummary(state: DesktopHarnessState, project: Project): Project {
  const assets = projectAssets(state, project.id);
  return {
    ...project,
    assetCount: assets.length,
    labeledCount: assets.filter((asset) => asset.labels.length > 0).length,
    removedCount: assets.filter((asset) => asset.removed).length,
  };
}

export function requireAsset(state: DesktopHarnessState, assetId: unknown): Asset {
  const asset = typeof assetId === "string" ? state.assets[assetId] : undefined;
  if (!asset) throw new Error(`Asset not found: ${String(assetId)}`);
  return asset;
}

/**
 * The single write path for an asset, so no handler can forget the order list
 * or leave `available` disagreeing with the representations.
 *
 * `available` is derived here for the same reason the catalog derives it
 * (`core/rcdvlog-core`: `let available = original_available || proxy_available`):
 * a clip whose original is on a disconnected volume is still reviewable from
 * its proxy, and that is the whole point of making proxies.
 */
export function putAsset(state: DesktopHarnessState, asset: Asset): Asset {
  const stored: Asset = { ...asset, available: asset.originalAvailable || asset.proxyAvailable };
  state.assets[stored.id] = stored;
  if (!state.assetOrder.includes(stored.id)) state.assetOrder.push(stored.id);
  return stored;
}

/**
 * Write a judgment and advance the clip's revision, the way the catalog does.
 *
 * The revision is what makes a stale write detectable, so a harness that left
 * it alone would answer "accepted" to a patch the real backend refuses — and
 * the CUJ tier's whole point is that it answers like the real one.
 */
export function patchAsset(
  state: DesktopHarnessState,
  assetId: string,
  patch: Partial<Asset>,
): Asset {
  const current = requireAsset(state, assetId);
  const judgment: Array<keyof Asset> = ["notes", "labels", "removed", "rotationDegrees"];
  const changed = judgment.some(
    (field) => field in patch && JSON.stringify(patch[field]) !== JSON.stringify(current[field]),
  );
  return putAsset(state, {
    ...current,
    ...patch,
    revision: changed ? current.revision + 1 : current.revision,
  });
}
