import { useCallback, useEffect, useMemo, useReducer, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { api } from "./api";
import type { ArchiveRoot, Asset, AssetPatch, AutoplayMode, BaselineImportResult, BaselinePreviewResult, CameraMapping, CameraMergePreview, CardCandidate, DesktopSettings, ImportHistoryEntry, ImportPreflight, ImportProgress, ImportRollbackPreview, ImportRunReport, ImportSessionDetail, PreflightItem, ModelInstallProgress, ModelStatus, PathAnalysis, Project, ProjectLibrary, ProjectLocation, ProjectPairingStatus, RootMappingEntry, ServerConnectionStatus, ServerVersionInfo, SyncStatus, TranscriptStatus } from "./types";
import {
  detectsSourceAutomatically,
  interruptedMenuNote,
  interruptedProgressNote,
  interruptedSourceHeadline,
  interruptionFromReport,
  isSameCardSession,
  isSameInterruptedSession,
  matchesInterruptedSource,
  selectMountedCards,
  type InterruptedImport,
  type InterruptedImportOptions,
} from "./import/interruptedImport";
import { useProjectSession } from "./session/projectSession";
import { useAssetMutations } from "./session/assetMutations";
import { orderAssets, replaceAsset as replaceAssetInList, moveSelection as moveSelectionInOrder } from "./review/order";
import { resolveEscape, type EscapeLayer } from "./review/escape";
import { resolveAction } from "./review/keymap";
import { CalibrationPanel } from "./CalibrationPanel";
import { chronologyApi } from "./chronologyApi";
import { formatChronologyTime } from "./chronology/geometry";
import type { ChronologyOverview, TripPhase } from "./chronologyTypes";
import { EditPlanPanel } from "./EditPlanPanel";
import { RIBBON_ZOOM_DEFAULT, RIBBON_ZOOM_MAX, RIBBON_ZOOM_MIN, RIBBON_ZOOM_STEP, ribbonHeight } from "./review/layout";
import { StoryRibbon, assetContextKey, type ContextDirection, type ContextRun } from "./components/StoryRibbon";
import { INITIAL_VIEW, reduceView, type Mode } from "./review/presentation";
import { Splitter } from "./components/Splitter";
import {
  DEFAULT_PANEL_SIZES,
  clampPanel,
  loadPanelSizes,
  savePanelSizes,
  type PanelSizes,
} from "./review/panelSizes";
import { JudgmentPanel } from "./components/JudgmentPanel";
import { EvidenceShelf, type ShelfTab } from "./components/EvidenceShelf";
import { TransportBar } from "./components/TransportBar";
import { QueuePanel } from "./components/QueuePanel";
import { VideoStage } from "./components/VideoStage";
import { TopBar, type OperationProgress } from "./components/TopBar";
import { TripPhaseDialog, type TripPhaseFormValue } from "./components/TripPhaseDialog";
import { ImportDialog } from "./components/ImportDialog";
import { ImportHistoryDialog } from "./components/ImportHistoryDialog";
import { KeyboardShortcutsModal } from "./components/KeyboardShortcutsModal";
import { AnalysisDialog } from "./components/AnalysisDialog";
import { SyncDiagnosticsModal } from "./components/SyncDiagnosticsModal";
import { NewProjectModal } from "./components/NewProjectModal";
import { PairingWizardModal } from "./components/PairingWizardModal";
import { BaselineWizardModal } from "./components/BaselineWizardModal";
import { SettingsModal } from "./components/SettingsModal";
import { CardBanner } from "./components/CardBanner";
import { CameraMergeDialog } from "./components/CameraMergeDialog";
import { analysisStageLabels, formatCost } from "./review/format";
import { sourceVolumeHint } from "./review/summaries";
import { canonicalLabel, projectLabelVocabulary } from "./review/labelVocabulary";
import { nextMatch, transcriptMatches } from "./review/searchMatch";
import { rotationAfter } from "./review/rotation";
import { buildMediaBuffer } from "./review/mediaBuffer";


interface PhaseDialogState {
  phase: TripPhase | null;
  startIndex: number;
  endIndex: number;
  split?: {
    existingPhaseId: string;
    boundaryAssetId: string;
    direction: ContextDirection;
  };
}

interface ImportChronologyRefresh {
  key: string;
  projectId: string;
  generation: number;
  timer: number | null;
  inFlight: Promise<void> | null;
  dirty: boolean;
  finalizing: boolean;
  closed: boolean;
}

function initialProgress(kind: OperationProgress["kind"], total: number): OperationProgress {
  return {
    kind,
    completed: 0,
    total,
    completedWorkUnits: 0,
    totalWorkUnits: 0,
    workerCount: 1,
    estimatedRemainingSeconds: null,
  };
}

function App() {
  // Chronology and edit planning are modes, not a drawer: they are global,
  // sit-down operations that were being asked to work in a strip at the
  // bottom of the review screen (spec §6).
  const [view, dispatchView] = useReducer(reduceView, INITIAL_VIEW);
  const [projects, setProjects] = useState<Project[]>([]);
  const [projectId, setProjectId] = useState("");
  const [selectedId, setSelectedId] = useState("");
  const [query, setQuery] = useState("");
  // Where the last spoken-match jump landed, so the next press moves on.
  const spokenMatchRef = useRef(-1);
  const [showRemoved, setShowRemoved] = useState(false);
  const [labelDraft, setLabelDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [operationProgress, setOperationProgress] = useState<OperationProgress | null>(null);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [showCreate, setShowCreate] = useState(false);
  const [showMediaMenu, setShowMediaMenu] = useState(false);
  const [showShortcuts, setShowShortcuts] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [newProjectName, setNewProjectName] = useState("");
  const [library, setLibrary] = useState<ProjectLibrary>({ rootPath: "", projects: [] });
  const [settings, setSettings] = useState<DesktopSettings>({
    projectsRoot: "",
    nodeId: "node_desktop_pending",
    autoplayMode: "proxyAvailable",
    lastCatalogPath: null,
    lastProjectId: null,
    lastAssetId: null,
    lastContextUpdatedAt: null,
    makeProxiesOnImport: true,
    lastImportArchiveRootId: null,
    importArchiveRootIdsByCardSession: {},
  });
  const [isMuted, setIsMuted] = useState(true);
  const [isPlaying, setIsPlaying] = useState(false);
  const [playbackSeconds, setPlaybackSeconds] = useState(0);
  const [mediaDuration, setMediaDuration] = useState(0);
  const [isBuffering, setIsBuffering] = useState(false);
  const [offsetMinutes, setOffsetMinutes] = useState(0);
  const [offsetLabel, setOffsetLabel] = useState("Camera time change");
  const [chronologyZoom, setChronologyZoom] = useState(RIBBON_ZOOM_DEFAULT);
  const [chronologyFocusAssetId, setChronologyFocusAssetId] = useState<string | null>(null);
  const [phaseDialog, setPhaseDialog] = useState<PhaseDialogState | null>(null);
  const [phaseDialogBusy, setPhaseDialogBusy] = useState(false);
  const [phaseDialogError, setPhaseDialogError] = useState("");
  const [cameraMergeSource, setCameraMergeSource] = useState("");
  const [cameraMergeDestination, setCameraMergeDestination] = useState("");
  const [cameraMergePreview, setCameraMergePreview] = useState<CameraMergePreview | null>(null);
  const [cameraMergeBusy, setCameraMergeBusy] = useState(false);
  const [cameraMergeError, setCameraMergeError] = useState("");
  const [showImportDialog, setShowImportDialog] = useState(false);
  const [importProjectName, setImportProjectName] = useState("");
  const [importSource, setImportSource] = useState("");
  const [importCard, setImportCard] = useState<CardCandidate | null>(null);
  const [archiveRoots, setArchiveRoots] = useState<ArchiveRoot[]>([]);
  const [selectedArchiveRootId, setSelectedArchiveRootId] = useState("");
  const [importPlan, setImportPlan] = useState<ImportPreflight | null>(null);
  /** Existing project camera identities, for the import dialog's camera control (#71). */
  const [importKnownCameras, setImportKnownCameras] = useState<string[]>([]);
  /** The camera the user explicitly chose for the current import dialog session, if any. */
  const [importCameraOverride, setImportCameraOverride] = useState<string | null>(null);
  /** Every stored evidence-to-camera mapping for this project (#71). */
  const [importCameraMappings, setImportCameraMappings] = useState<CameraMapping[]>([]);
  const [importProgress, setImportProgress] = useState<ImportProgress | null>(null);
  const [importReport, setImportReport] = useState<ImportRunReport | null>(null);
  const [importLoading, setImportLoading] = useState(false);
  const [importRunning, setImportRunning] = useState(false);
  const [showImportHistory, setShowImportHistory] = useState(false);
  const [importSessions, setImportSessions] = useState<ImportHistoryEntry[]>([]);
  const [importSessionDetail, setImportSessionDetail] = useState<ImportSessionDetail | null>(null);
  const [importRollback, setImportRollback] = useState<ImportRollbackPreview | null>(null);
  const [importError, setImportError] = useState("");
  const [importHistoryError, setImportHistoryError] = useState("");
  const [verifyImportCopies, setVerifyImportCopies] = useState(true);
  const [makeImportProxies, setMakeImportProxies] = useState(true);
  const [mountedCard, setMountedCard] = useState<CardCandidate | null>(null);
  const dismissedCardSessionsRef = useRef(new Set<string>());
  /**
   * The Smart ingest session whose source disappeared mid-import (#67). It
   * outlives the dialog: the media menu reopens it, and mount polling watches
   * for its card by session identity so a remount is recognized on its own.
   */
  const [interruptedImport, setInterruptedImport] = useState<InterruptedImport | null>(null);
  /** That session's own card, back in the slot. Identity-matched, never by path. */
  const [resumableCard, setResumableCard] = useState<CardCandidate | null>(null);
  /** Whether the import dialog is currently showing the waiting-for-source state. */
  const [importDisconnected, setImportDisconnected] = useState(false);
  // The active shelf tab is a mode, not per-clip state: it persists as you
  // move through the queue (spec §3.6).
  const [shelfTab, setShelfTab] = useState<ShelfTab>("transcript");
  const [panelSizes, setPanelSizes] = useState<PanelSizes>(() =>
    loadPanelSizes(typeof window === "undefined" ? null : window.localStorage),
  );

  const resizePanel = useCallback((kind: keyof PanelSizes, value: number) => {
    const viewport = kind === "ribbon" ? window.innerHeight : window.innerWidth;
    setPanelSizes((current) => ({ ...current, [kind]: clampPanel(kind, value, viewport) }));
  }, []);

  // Written on drag end rather than on every pointer move: a drag fires
  // hundreds of moves and none of the intermediate sizes are worth storing.
  const commitPanelSizes = useCallback(() => {
    setPanelSizes((current) => {
      savePanelSizes(typeof window === "undefined" ? null : window.localStorage, current);
      return current;
    });
  }, []);

  // ── Clip analysis state ────────────────────────────────────────────
  const [showAnalysisDialog, setShowAnalysisDialog] = useState(false);
  const [budgetInput, setBudgetInput] = useState("1.00");
  const [forceReanalyze, setForceReanalyze] = useState(false);
  const [showUnusableOnly, setShowUnusableOnly] = useState(false);
  const [models, setModels] = useState<ModelStatus[]>([]);
  const [modelInstall, setModelInstall] = useState<ModelInstallProgress | null>(null);
  const [keyPresent, setKeyPresent] = useState(false);
  const [keyInput, setKeyInput] = useState("");

  // ── Server pairing / sync state ────────────────────────────────────
  const [serverStatus, setServerStatus] = useState<ServerConnectionStatus | null>(null);
  const [showPairingWizard, setShowPairingWizard] = useState(false);
  const [pairingWizardStep, setPairingWizardStep] = useState<"endpoint" | "code" | "connecting">("endpoint");
  const [serverEndpoint, setServerEndpoint] = useState("");
  const [pairingCode, setPairingCode] = useState("");
  const [deviceLabel, setDeviceLabel] = useState("MacBook Pro");
  const [serverIdentity, setServerIdentity] = useState<ServerVersionInfo | null>(null);

  // ── Sync state ─────────────────────────────────────────────────────
  const [syncStatus, setSyncStatus] = useState<SyncStatus | null>(null);
  const [showSyncDiagnostics, setShowSyncDiagnostics] = useState(false);

  // ── Baseline pairing wizard (per-project) ─────────────────────────
  const [showBaselineWizard, setShowBaselineWizard] = useState(false);
  const [baselineStep, setBaselineStep] = useState<"root_mapping" | "preview" | "confirming" | "complete" | "error">("root_mapping");
  const [pathAnalysis, setPathAnalysis] = useState<PathAnalysis | null>(null);
  const [rootMappings, setRootMappings] = useState<RootMappingEntry[]>([]);
  const [baselinePreview, setBaselinePreview] = useState<BaselinePreviewResult | null>(null);
  const [baselineError, setBaselineError] = useState("");
  const [pairingProjectId, setPairingProjectId] = useState("");
  const activeOperationRef = useRef<OperationProgress["kind"] | null>(null);
  const notesEditingRef = useRef<{ assetId: string; savedNotes: string; cancelOnBlur: boolean } | null>(null);
  const chronologyScrollRef = useRef<HTMLDivElement>(null);
  const reviewRibbonScrollLeftRef = useRef(0);
  const reviewQueueScrollTopRef = useRef(0);
  const chronologyReviewSnapshotRef = useRef(false);
  const chronologyPreserveRibbonViewportRef = useRef(false);
  const queueRef = useRef<HTMLDivElement>(null);
  const viewerStageRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const assetsRef = useRef<Asset[]>([]);
  const projectIdRef = useRef("");
  const selectedIdRef = useRef("");
  const importChronologyGenerationRef = useRef({ projectId: "", generation: 0 });
  const importChronologyRefreshesRef = useRef(new Map<string, ImportChronologyRefresh>());
  // The clip to land on when a queue arrives with no live selection yet — the
  // review position recorded on last exit. Held in a ref so the assets read can
  // consult it without a dependency, and left in place: once the selection is
  // live, `onAssetsRead` keeps it and never consults this again.
  const restoreAssetIdRef = useRef("");
  const orientationQueueRef = useRef<string[]>([]);
  const orientationActiveRef = useRef(new Set<string>());
  const cameraMergeRequestRef = useRef(0);
  // Event listeners are registered once and can fire between renders and
  // effects; keep the project guard current synchronously with the render.
  projectIdRef.current = projectId;
  if (importChronologyGenerationRef.current.projectId !== projectId) {
    importChronologyGenerationRef.current = {
      projectId,
      generation: importChronologyGenerationRef.current.generation + 1,
    };
  }

  useEffect(() => {
    cameraMergeRequestRef.current += 1;
    setCameraMergeSource("");
    setCameraMergeDestination("");
    setCameraMergePreview(null);
    setCameraMergeError("");
  }, [projectId]);

  // Close only this project's generation on a switch. A held old-project
  // solve therefore cannot reset or schedule the new project's work.
  useEffect(() => {
    const { projectId: effectProjectId, generation } = importChronologyGenerationRef.current;
    const key = `${effectProjectId}:${generation}`;
    return () => {
      const refresh = importChronologyRefreshesRef.current.get(key);
      if (!refresh) return;
      refresh.closed = true;
      refresh.dirty = false;
      if (refresh.timer !== null) {
        window.clearTimeout(refresh.timer);
        refresh.timer = null;
      }
    };
  }, [projectId]);

  // One owner for the open project's reads, and the only thing that may write
  // them: a response from a project the user has since left is dropped rather
  // than allowed to overwrite the session on screen.
  const session = useProjectSession({
    projectId,
    selectedId,
    onError: setError,
    onAssetsRead: useCallback((loaded: Asset[]) => {
      // Applied with the queue rather than in a following effect, so the panel
      // and the queue arrive in the same render. With no live selection yet,
      // prefer the recorded review position when it is still in this queue, and
      // fall back to the first clip when it is gone or belongs elsewhere.
      setSelectedId((current) => {
        if (loaded.some((asset) => asset.id === current)) return current;
        const restore = restoreAssetIdRef.current;
        if (restore && loaded.some((asset) => asset.id === restore)) return restore;
        return loaded[0]?.id || "";
      });
      // A count here would be the third copy of a number the queue badge and
      // the ribbon already carry; only the empty case says anything new.
      setMessage(loaded.length ? "" : "Choose a media folder to begin");
    }, []),
  });
  const { analysisIndex, readiness, transcript, observation, patchAssets } = session;
  const applyAssetToQueue = session.applyAsset;
  const upsertSessionAsset = session.upsertAsset;
  const chronologyOverview = session.chronology;
  const reloadSession = session.reload;
  const reloadSessionAssets = session.reloadAssets;
  const reloadSessionChronology = session.reloadChronology;

  const scheduleImportChronologyRefresh = useCallback((importedProjectId: string) => {
    const current = importChronologyGenerationRef.current;
    if (current.projectId !== importedProjectId) return;
    const key = `${importedProjectId}:${current.generation}`;
    let refresh = importChronologyRefreshesRef.current.get(key);
    if (!refresh || refresh.closed) {
      refresh = {
        key,
        projectId: importedProjectId,
        generation: current.generation,
        timer: null,
        inFlight: null,
        dirty: false,
        finalizing: false,
        closed: false,
      };
      importChronologyRefreshesRef.current.set(key, refresh);
    }
    if (refresh.finalizing) {
      // The final authoritative refresh is already serialized behind any
      // progressive work; let it absorb this late event rather than overlap.
      refresh.dirty = true;
      return;
    }
    if (refresh.timer !== null) window.clearTimeout(refresh.timer);
    refresh.timer = window.setTimeout(() => {
      refresh!.timer = null;
      const active = importChronologyGenerationRef.current;
      if (
        refresh!.closed
        || active.projectId !== refresh!.projectId
        || active.generation !== refresh!.generation
      ) return;
      if (refresh!.inFlight) {
        refresh!.dirty = true;
        return;
      }
      const flight = reloadSessionChronology();
      refresh!.inFlight = flight;
      void flight.finally(() => {
        if (refresh!.inFlight !== flight) return;
        refresh!.inFlight = null;
        const latest = importChronologyGenerationRef.current;
        if (
          refresh!.closed
          || latest.projectId !== refresh!.projectId
          || latest.generation !== refresh!.generation
        ) {
          refresh!.dirty = false;
          return;
        }
        if (refresh!.finalizing) return;
        if (refresh!.dirty) {
          refresh!.dirty = false;
          scheduleImportChronologyRefresh(refresh!.projectId);
        }
      });
    }, 100);
  }, [reloadSessionChronology]);

  const reconcileImportChronology = useCallback(async (importingProjectId: string) => {
    const current = importChronologyGenerationRef.current;
    if (current.projectId !== importingProjectId) return;
    const key = `${importingProjectId}:${current.generation}`;
    let refresh = importChronologyRefreshesRef.current.get(key);
    if (!refresh || refresh.closed) {
      refresh = {
        key,
        projectId: importingProjectId,
        generation: current.generation,
        timer: null,
        inFlight: null,
        dirty: false,
        finalizing: false,
        closed: false,
      };
      importChronologyRefreshesRef.current.set(key, refresh);
    }
    refresh.finalizing = true;
    refresh.dirty = false;
    if (refresh.timer !== null) {
      window.clearTimeout(refresh.timer);
      refresh.timer = null;
    }
    try {
      await refresh.inFlight;
      const latest = importChronologyGenerationRef.current;
      if (
        refresh.closed
        || latest.projectId !== importingProjectId
        || latest.generation !== refresh.generation
      ) return;
      // Do not race the final authoritative query with the progressive read
      // it supersedes. This direct read is deliberately awaited by the caller.
      await reloadSessionChronology();
    } finally {
      refresh.finalizing = false;
      const latest = importChronologyGenerationRef.current;
      const needsFollowUp = refresh.dirty
        && !refresh.closed
        && latest.projectId === refresh.projectId
        && latest.generation === refresh.generation;
      refresh.dirty = false;
      if (needsFollowUp) scheduleImportChronologyRefresh(refresh.projectId);
    }
  }, [reloadSessionChronology, scheduleImportChronologyRefresh]);

  // One owner for the write path: optimistic apply, the stated failure policy,
  // save state, and the double-submit guard.
  const mutations = useAssetMutations({
    update: useCallback((assetId: string, patch: AssetPatch) => api.updateAsset(assetId, patch), []),
    reread: useCallback((assetId: string) => api.getAsset(assetId), []),
    applyAsset: applyAssetToQueue,
    onError: setError,
  });

  // An unsaved note draft is not in the catalog, so it is written back over
  // whatever a read returns. Without this a reload would quietly replace the
  // user's paragraph with the value the backend still holds.
  const { lifecycle: mutationLifecycle } = mutations;
  const assets = useMemo(
    () => mutationLifecycle.withDrafts(session.assets),
    [mutationLifecycle, mutations.drafts, session.assets],
  );

  // Built from the session's assets rather than `assets`: label writes land
  // there optimistically, while the draft overlay changes on every keystroke
  // in the notes field — and rebuilding a few thousand clips' vocabulary per
  // character typed would be work for nothing. Removed clips count: their
  // labels are still part of what this project calls things.
  const labelVocabulary = useMemo(() => projectLabelVocabulary(session.assets), [session.assets]);

  const selected = assets.find((asset) => asset.id === selectedId) ?? null;
  const project = projects.find((item) => item.id === projectId) ?? null;

  useEffect(() => {
    assetsRef.current = assets;
    selectedIdRef.current = selectedId;
  }, [assets, selectedId]);

  // Another clip or another phrase starts the cycle again.
  useEffect(() => { spokenMatchRef.current = -1; }, [query, selectedId]);

  const activeAssets = useMemo(() => orderAssets(assets, { removedVisible: false }), [assets]);
  const filteredAssets = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    let queueAssets = orderAssets(assets, { removedVisible: showRemoved });
    if (showUnusableOnly) {
      // Clips with no observation are never hidden here: an unfiltered clip is
      // one analysis has not judged, not one it approved.
      queueAssets = queueAssets.filter((asset) => analysisIndex[asset.id]?.usable === false);
    }
    if (!needle) return queueAssets;
    return queueAssets.filter((asset) =>
      [asset.filename, asset.title, asset.description, asset.camera, asset.notes, analysisIndex[asset.id]?.transcriptText ?? "", ...asset.labels]
        .join(" ")
        .toLocaleLowerCase()
        .includes(needle),
    );
  }, [analysisIndex, assets, query, showRemoved, showUnusableOnly]);

  const unusableCount = useMemo(
    () => assets.filter((asset) => !asset.removed && analysisIndex[asset.id]?.usable === false).length,
    [analysisIndex, assets],
  );

  const lanes = useMemo(() => {
    const grouped = new Map<string, Asset[]>();
    activeAssets.forEach((asset) => {
      grouped.set(asset.camera, [...(grouped.get(asset.camera) ?? []), asset]);
    });
    return [...grouped.entries()];
  }, [activeAssets]);

  // Merge actions stay attached to visible Review lanes, but a valid
  // destination may currently be represented only by removed footage or an
  // empty chronology card. Keep those identities reachable and deterministic.
  const projectCameraIdentities = useMemo(() => {
    const identities = new Set(assets.map((asset) => asset.camera));
    chronologyOverview?.sequences.forEach((sequence) => identities.add(sequence.cameraKey));
    return [...identities].sort((left, right) => left.localeCompare(right));
  }, [assets, chronologyOverview]);

  const chronologyAssetsById = useMemo(
    () => new Map((chronologyOverview?.assets ?? []).map((asset) => [asset.assetId, asset])),
    [chronologyOverview],
  );
  const phasesById = useMemo(
    () => new Map((chronologyOverview?.phases ?? []).map((phase) => [phase.id, phase])),
    [chronologyOverview],
  );
  const chronologyProblemCount = (chronologyOverview?.contradictions.length ?? 0)
    + ((chronologyOverview?.unplacedAssets ?? 0) > 0 ? 1 : 0);
  /**
   * Counter restarts are counted apart from problems and never as one. Swapping
   * cards through a camera is ordinary practice, so a warning chip over it
   * reports a fault in footage that is usually perfectly fine — while still
   * needing to be reachable, because only the user knows the answer.
   */
  const chronologyQuestionCount = chronologyOverview?.proposedSplits.length ?? 0;
  const firstChronologyProblemAssetId = chronologyOverview?.contradictions
    .find((problem) => problem.assetIds.length > 0)?.assetIds[0]
    ?? chronologyOverview?.assets.find((asset) => !asset.tripTime)?.assetId
    ?? chronologyOverview?.proposedSplits.find((proposal) => proposal.affectedAssetIds.length > 0)?.affectedAssetIds[0]
    ?? selectedId
    ?? null;
  const phaseIdByAsset = useMemo(() => {
    const index = new Map<string, string>();
    chronologyAssetsById.forEach((chronology, assetId) => {
      if (chronology.displayTime?.phaseId) index.set(assetId, chronology.displayTime.phaseId);
    });
    return index;
  }, [chronologyAssetsById]);

  const contextRuns = useMemo(() => {
    const runs: ContextRun[] = [];
    let previousKey = "";
    activeAssets.forEach((asset, index) => {
      const phaseId = phaseIdByAsset.get(asset.id) ?? null;
      const phase = phaseId ? phasesById.get(phaseId) : null;
      const hasLegacyContext = [asset.phase, asset.location, asset.timezone, asset.storyBeat]
        .some((value) => Boolean(value?.trim()));
      if (!phase && !hasLegacyContext) {
        previousKey = "";
        return;
      }
      const displayTimezone = chronologyAssetsById.get(asset.id)?.displayTime?.timezone ?? null;
      const contextAsset = phase
        ? {
            ...asset,
            phase: phase.label,
            location: phase.kind === "location" ? phase.label : null,
            timezone: displayTimezone ?? phase.timezone,
            storyBeat: null,
          }
        : asset;
      const key = phase ? `phase:${phase.id}` : `legacy:${assetContextKey(asset)}`;
      const previous = runs.at(-1);
      if (previous && previousKey === key && previous.end === index - 1) {
        previous.end = index;
      } else {
        runs.push({ start: index, end: index, asset: contextAsset, phaseId });
      }
      previousKey = key;
    });
    return runs;
  }, [activeAssets, chronologyAssetsById, phaseIdByAsset, phasesById]);

  const refreshProjects = useCallback(async () => {
    const result = await api.listProjects();
    setProjects(result);
    setProjectId((current) => current || result[0]?.id || "");
    if (result.length === 0) setShowCreate(true);
  }, []);

  const refreshLibrary = useCallback(async () => {
    const result = await api.projectLibrary();
    setLibrary(result);
    return result;
  }, []);

  useEffect(() => {
    Promise.all([refreshProjects(), refreshLibrary(), api.prerequisiteStatus(), api.desktopSettings()])
      .then(([, , tools, desktopSettings]) => {
        setSettings(desktopSettings);
        // Set before the assets read commits, so the first queue can land on
        // the recorded clip rather than the first one.
        restoreAssetIdRef.current = desktopSettings.lastAssetId ?? "";
        setMakeImportProxies(desktopSettings.makeProxiesOnImport);
        api.serverConnectionStatus().then(setServerStatus).catch(() => {});
        const missing = tools.filter((tool) => !tool.available).map((tool) => tool.name);
        if (missing.length) {
          setError(`Missing prerequisites: ${missing.join(", ")}. Install them and relaunch.`);
        }
      })
      .catch((reason) => {
        setError(String(reason));
        setMessage("Catalog unavailable");
      });
  }, [refreshLibrary, refreshProjects]);

  useEffect(() => {
    // An interrupted import belongs to the project that started it (#67). It
    // survives a project switch — its clips are still half-copied and its card
    // is still missing — but its waiting screen must not stay on top of a
    // different project's review.
    if (interruptedImport && interruptedImport.projectId !== projectId) {
      setImportDisconnected(false);
    }
  }, [interruptedImport, projectId]);

  useEffect(() => {
    // Import sessions are project-scoped state, just like the queue. An older
    // catalog read must not replace the history after a project switch.
    let active = true;
    const requestedProjectId = projectId;
    api.listImportSessions(requestedProjectId)
      .then((sessions) => {
        if (active && projectIdRef.current === requestedProjectId) setImportSessions(sessions);
      })
      .catch(() => {
        if (active && projectIdRef.current === requestedProjectId) setImportSessions([]);
      });
    return () => { active = false; };
  }, [projectId]);

  useEffect(() => {
    void api.modelStatus().then(setModels).catch(() => setModels([]));
    void api.openRouterKeyPresent().then(setKeyPresent).catch(() => setKeyPresent(false));
  }, []);

  // ── Sync status listener ──────────────────────────────────────────
  useEffect(() => {
    const unlisten = api.onSyncStatusChanged((status) => {
      setSyncStatus((current) => {
        if (current && current.projectId === status.projectId) return status;
        return current;
      });
    });
    return () => {
      unlisten.then((fn) => fn()).catch(() => {});
    };
  }, []);

  // Load sync status when project changes
  useEffect(() => {
    if (!projectId) {
      setSyncStatus(null);
      return;
    }
    api.syncStatus(projectId).then(setSyncStatus).catch(() => {});
  }, [projectId]);

  useEffect(() => {
    if (!projectId) return;
    const timeout = window.setTimeout(() => {
      api.recordDesktopContext(projectId, selectedId || null).catch((reason) => setError(String(reason)));
    }, 100);
    return () => window.clearTimeout(timeout);
  }, [projectId, selectedId]);

  const pumpOrientationQueue = useCallback(() => {
    while (orientationActiveRef.current.size < 2) {
      const assetId = orientationQueueRef.current.shift();
      if (!assetId) return;
      if (orientationActiveRef.current.has(assetId)) continue;
      const asset = assetsRef.current.find((candidate) => candidate.id === assetId);
      if (!asset || asset.orientationProbed || !asset.originalAvailable) continue;
      orientationActiveRef.current.add(assetId);
      void api.refreshAssetOrientation(assetId)
        .then((updated) => {
          patchAssets((current) => replaceAssetInList(current, updated));
        })
        .catch((reason) => {
          if (selectedIdRef.current === assetId) setError(String(reason));
        })
        .finally(() => {
          orientationActiveRef.current.delete(assetId);
          pumpOrientationQueue();
        });
    }
  }, [patchAssets]);

  useEffect(() => {
    const selectedIndex = filteredAssets.findIndex((asset) => asset.id === selectedId);
    if (selectedIndex < 0) return;
    const candidates = [selectedIndex, selectedIndex + 1, selectedIndex + 2, selectedIndex - 1]
      .map((index) => filteredAssets[index])
      .filter((asset): asset is Asset => Boolean(asset && !asset.orientationProbed && asset.originalAvailable))
      .map((asset) => asset.id)
      .filter((assetId, index, ids) => ids.indexOf(assetId) === index && !orientationActiveRef.current.has(assetId));
    orientationQueueRef.current = candidates;
    pumpOrientationQueue();
  }, [filteredAssets, pumpOrientationQueue, selectedId]);

  useEffect(() => {
    const unlisten: Array<() => void> = [];
    let disposed = false;
    const attach = async () => {
      const listeners = await Promise.all([
        api.onProxyProgress((progress) => {
          if (progress.asset) {
            patchAssets((current) => progress.asset ? replaceAssetInList(current, progress.asset) : current);
          }
          if (activeOperationRef.current === "proxy") {
            setOperationProgress({
              kind: "proxy",
              completed: progress.estimate.completedItems,
              total: progress.estimate.totalItems,
              completedWorkUnits: progress.estimate.completedWorkUnits,
              totalWorkUnits: progress.estimate.totalWorkUnits,
              workerCount: progress.estimate.workerCount,
              estimatedRemainingSeconds: progress.estimate.estimatedRemainingSeconds,
            });
          }
        }),
        api.onThumbnailProgress((progress) => {
          if (progress.asset) {
            patchAssets((current) => progress.asset ? replaceAssetInList(current, progress.asset) : current);
          }
          if (activeOperationRef.current === "thumbnail") {
            setOperationProgress({
              kind: "thumbnail",
              completed: progress.estimate.completedItems,
              total: progress.estimate.totalItems,
              completedWorkUnits: progress.estimate.completedWorkUnits,
              totalWorkUnits: progress.estimate.totalWorkUnits,
              workerCount: progress.estimate.workerCount,
              estimatedRemainingSeconds: progress.estimate.estimatedRemainingSeconds,
            });
          }
        }),
        api.onAnalysisProgress((progress) => {
          // Replacing the asset as each clip lands is what makes a generated
          // title appear during the run rather than after it.
          if (progress.asset) {
            patchAssets((current) => progress.asset ? replaceAssetInList(current, progress.asset) : current);
          }
          if (activeOperationRef.current === "analysis") {
            setOperationProgress({
              kind: "analysis",
              completed: progress.estimate.completedItems,
              total: progress.estimate.totalItems,
              completedWorkUnits: progress.estimate.completedWorkUnits,
              totalWorkUnits: progress.estimate.totalWorkUnits,
              workerCount: progress.estimate.workerCount,
              estimatedRemainingSeconds: progress.estimate.estimatedRemainingSeconds,
              stage: progress.stage,
              stageIndex: progress.stageIndex,
              stageCount: progress.stageCount,
              costUsd: progress.costUsd,
            });
          }
        }),
        api.onModelInstallProgress((progress) => setModelInstall(progress)),
        api.onImportProgress((progress) => {
          // Import progress is emitted from a long-lived native worker. A
          // user may have switched projects since it started, in which case
          // neither its progress UI nor a committed asset belongs here.
          if (progress.projectId !== projectIdRef.current) return;
          setImportProgress(progress);
          // A Smart-import item is safe to show only when the Rust-owned
          // import loop has committed it and supplied the materialized asset.
          // Insert it into the cache in canonical review order instead of
          // waiting for the end-of-run reconciliation. The project guard is a
          // ref because event listeners outlive the render that attached them.
          if (progress.asset) {
            upsertSessionAsset(progress.asset);
            // The ribbon can render the committed asset immediately from the
            // queue cache. Solver-derived context follows in one coalesced,
            // single-flight refresh rather than once per copied clip.
            scheduleImportChronologyRefresh(progress.asset.projectId);
          }
          if (activeOperationRef.current === "import" && progress.stage !== "complete") {
            setOperationProgress({
              kind: "import",
              completed: progress.completed,
              total: progress.total,
              completedWorkUnits: progress.completedBytes,
              totalWorkUnits: progress.totalBytes,
              workerCount: 1,
              estimatedRemainingSeconds: null,
              importStage: progress.stage,
              currentFile: progress.currentFile,
            });
          }
        }),
      ]);
      if (disposed) listeners.forEach((dispose) => dispose());
      else unlisten.push(...listeners);
    };
    void attach();
    return () => {
      disposed = true;
      unlisten.forEach((dispose) => dispose());
      for (const refresh of importChronologyRefreshesRef.current.values()) {
        refresh.closed = true;
        refresh.dirty = false;
        if (refresh.timer !== null) {
          window.clearTimeout(refresh.timer);
          refresh.timer = null;
        }
      }
    };
  }, [patchAssets, scheduleImportChronologyRefresh, upsertSessionAsset]);

  // Mount detection is deliberately low-frequency and listing-only. It offers
  // a review action; hashing and probing begin only after that click.
  //
  // The one poll answers two questions (`selectMountedCards`): what the banner
  // should offer, and whether an interrupted import's own card is back. The
  // second answer is why the poll keeps running while the import dialog is
  // open and why it ignores the dismissed set for that one session — a card
  // enters the dismissed set the moment the user chooses to review it, which
  // is exactly the card a subsequent disconnect needs to notice returning.
  useEffect(() => {
    if (!api.isDesktop() || !projectId) {
      setMountedCard(null);
      setResumableCard(null);
      return;
    }
    let disposed = false;
    const waitingForSource = detectsSourceAutomatically(interruptedImport);
    const poll = async () => {
      try {
        const cards = await api.detectMountedCards();
        if (disposed) return;
        const { banner, resumable } = selectMountedCards(
          cards,
          dismissedCardSessionsRef.current,
          interruptedImport,
        );
        setResumableCard(resumable);
        // The banner is a background offer; it stays put while a modal owns
        // the screen rather than changing behind it.
        if (!showImportDialog) setMountedCard(banner);
      } catch {
        if (!disposed) {
          setMountedCard(null);
          setResumableCard(null);
        }
      }
    };
    void poll();
    // Someone standing at the machine reseating a cable should not wait out a
    // 15-second cycle, so the wait for a known-missing source is tightened —
    // still listing-only, and still bounded to the interruption.
    const timer = window.setInterval(() => void poll(), waitingForSource ? 4_000 : 15_000);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [interruptedImport, projectId, showImportDialog]);

  useEffect(() => {
    if (selected) {
      setOffsetMinutes(Math.round(selected.offsetSeconds / 60));
    }
  }, [selected?.id, selected?.offsetSeconds]);

  useEffect(() => {
    queueRef.current
      ?.querySelector<HTMLElement>('[aria-selected="true"]')
      ?.scrollIntoView({ block: "nearest" });
  }, [selectedId]);

  useEffect(() => {
    if (filteredAssets.some((asset) => asset.id === selectedId)) return;
    setSelectedId(filteredAssets[0]?.id ?? "");
  }, [filteredAssets, selectedId]);

  useEffect(() => setLabelDraft(""), [selectedId]);

  // Entering Focus with F puts the caret in the notes field, since writing is
  // what you came for. Entering bare (the expand control) does not, because
  // focusing an invisible field would strand the caret.
  useEffect(() => {
    if (view.presentation !== "focus" || !view.overlays) return;
    document.querySelector<HTMLTextAreaElement>("#notes")?.focus();
  }, [view.presentation, view.overlays]);

  // Status messages are transient. The old status line kept the last one on
  // screen indefinitely, so it was usually stale by the time you read it.
  useEffect(() => {
    if (!message) return;
    const timer = setTimeout(() => setMessage(""), 4000);
    return () => clearTimeout(timer);
  }, [message]);

  /**
   * Put an asset on screen without writing it: a local edit, a progress event,
   * or an optimistic value. An asset carrying an unsaved note revises that
   * draft rather than being overwritten by it, so typing into a field whose
   * last save was refused still changes what a retry would send.
   */
  const replaceAsset = useCallback((updated: Asset) => {
    applyAssetToQueue(updated);
    mutationLifecycle.reviseDraft(updated);
  }, [applyAssetToQueue, mutationLifecycle]);

  /// Commits an edited title/description. Separate from the mutation lifecycle
  /// because these are not part of the full-replacement `AssetPatch` contract.
  const saveClipSummary = useCallback(async (asset: Asset) => {
    try {
      replaceAsset(await api.setClipSummary(asset.id, asset.title, asset.description));
    } catch (reason) {
      setError(String(reason));
    }
  }, [replaceAsset]);

  const persistAsset = useCallback(
    async (asset: Asset, changes: Partial<AssetPatch>) => {
      await mutationLifecycle.submit(asset, changes);
    },
    [mutationLifecycle],
  );

  /** A note save: the draft stays on screen and stays marked if it is refused. */
  const persistNotes = useCallback(
    async (asset: Asset) => {
      await mutationLifecycle.saveNotes(asset);
    },
    [mutationLifecycle],
  );

  const addLabel = useCallback((asset: Asset, value: string) => {
    // The project's own spelling wins over a new casing of it (issue #11):
    // typing "Market" where "market" exists joins that label rather than
    // opening a second one beside it. A word the project has not seen is
    // written exactly as typed.
    const label = canonicalLabel(labelVocabulary, value);
    if (!label) return;
    setLabelDraft("");
    if (asset.labels.some((current) => current.toLocaleLowerCase() === label.toLocaleLowerCase())) return;
    void persistAsset(asset, { labels: [...asset.labels, label] });
  }, [labelVocabulary, persistAsset]);

  const removeLabel = useCallback((asset: Asset, label: string) => {
    void persistAsset(asset, {
      labels: asset.labels.filter((current) => current.toLocaleLowerCase() !== label.toLocaleLowerCase()),
    });
  }, [persistAsset]);

  const toggleRemoved = useCallback((asset: Asset) => {
    const index = filteredAssets.findIndex((candidate) => candidate.id === asset.id);
    const nextId = filteredAssets[index + 1]?.id ?? filteredAssets[index - 1]?.id ?? "";
    void persistAsset(asset, { removed: !asset.removed });
    if (asset.removed && filteredAssets.length === 1) setShowRemoved(false);
    setSelectedId(nextId);
  }, [filteredAssets, persistAsset]);

  const moveSelection = useCallback(
    (delta: number) => {
      if (!filteredAssets.length) return;
      const next = moveSelectionInOrder(filteredAssets, selectedId || null, delta);
      if (next !== null) setSelectedId(next);
    },
    [filteredAssets, selectedId],
  );

  const centerSelectedInChronology = useCallback((behavior: ScrollBehavior = "smooth") => {
    const scroll = chronologyScrollRef.current;
    const index = activeAssets.findIndex((asset) => asset.id === selectedId);
    if (!scroll || index < 0) return;
    const clipCenter = index * chronologyZoom + chronologyZoom / 2;
    scroll.scrollTo({ left: Math.max(0, clipCenter - scroll.clientWidth / 2), behavior });
  }, [activeAssets, chronologyZoom, selectedId]);

  const openChronology = useCallback((assetId?: string | null, preserveViewport = true) => {
    if (view.mode === "review") {
      reviewRibbonScrollLeftRef.current = chronologyScrollRef.current?.scrollLeft ?? 0;
      reviewQueueScrollTopRef.current = queueRef.current?.scrollTop ?? 0;
      chronologyReviewSnapshotRef.current = true;
      chronologyPreserveRibbonViewportRef.current = preserveViewport;
    } else if (view.mode !== "chronology") {
      chronologyPreserveRibbonViewportRef.current = false;
    }
    setChronologyFocusAssetId(assetId || selectedId || null);
    if (view.mode !== "chronology") dispatchView({ kind: "setMode", mode: "chronology" });
  }, [selectedId, view.mode]);

  const returnToReview = useCallback(() => {
    if (view.mode === "review") return;
    const restoreChronologySnapshot = view.mode === "chronology" && chronologyReviewSnapshotRef.current;
    dispatchView({ kind: "setMode", mode: "review" });
    if (!restoreChronologySnapshot) return;
    chronologyReviewSnapshotRef.current = false;
    window.requestAnimationFrame(() => {
      if (chronologyScrollRef.current) {
        chronologyScrollRef.current.scrollLeft = reviewRibbonScrollLeftRef.current;
      }
      if (queueRef.current) {
        queueRef.current.scrollTop = reviewQueueScrollTopRef.current;
      }
    });
  }, [view.mode]);

  useEffect(() => {
    centerSelectedInChronology("smooth");
  }, [centerSelectedInChronology]);

  const openContextRange = useCallback((assetId: string, direction: ContextDirection, source: ContextRun | null) => {
    const anchor = activeAssets.findIndex((asset) => asset.id === assetId);
    if (anchor < 0) return;
    const hasPlacedTime = (index: number) => {
      const asset = activeAssets[index];
      return Boolean(asset && (chronologyAssetsById.get(asset.id)?.tripTime || asset.bestTime));
    };
    if (!hasPlacedTime(anchor)) {
      setError("Place this clip in Chronology before using it as a context boundary.");
      return;
    }
    if (source?.phaseId) {
      setPhaseDialog({
        phase: null,
        startIndex: direction === "through" ? source.start : anchor,
        endIndex: direction === "through" ? anchor : source.end,
        split: {
          existingPhaseId: source.phaseId,
          boundaryAssetId: assetId,
          direction,
        },
      });
      setPhaseDialogError("");
      return;
    }
    if (direction === "through") {
      const previous = contextRuns.filter((run) => run.end < anchor).at(-1);
      let startIndex = previous ? previous.end + 1 : 0;
      while (startIndex < anchor && !hasPlacedTime(startIndex)) startIndex += 1;
      setPhaseDialog({ phase: null, startIndex, endIndex: anchor });
    } else {
      const next = contextRuns.find((run) => run.start > anchor);
      let endIndex = next ? next.start - 1 : activeAssets.length - 1;
      while (endIndex > anchor && !hasPlacedTime(endIndex)) endIndex -= 1;
      setPhaseDialog({
        phase: null,
        startIndex: anchor,
        endIndex,
      });
    }
    setPhaseDialogError("");
  }, [activeAssets, chronologyAssetsById, contextRuns]);

  const openExistingContext = useCallback((phaseId: string, startIndex: number, endIndex: number) => {
    const phase = phasesById.get(phaseId);
    if (!phase) return;
    setPhaseDialog({ phase, startIndex, endIndex });
    setPhaseDialogError("");
  }, [phasesById]);

  async function saveTripPhase(
    value: TripPhaseFormValue,
    range?: { startPosition: number; endPosition: number },
  ) {
    if (!phaseDialog || !projectId) return;
    const start = activeAssets[range?.startPosition ?? phaseDialog.startIndex];
    const end = activeAssets[range?.endPosition ?? phaseDialog.endIndex];
    if (!start || !end) return;
    const existing = phaseDialog.phase;
    setPhaseDialogBusy(true);
    setPhaseDialogError("");
    try {
      const draft: TripPhase = {
        id: existing?.id ?? "",
        label: value.label,
        kind: value.kind,
        startAnchorAssetId: start.id,
        endAnchorAssetId: end.id,
        startTripTime: start.bestTime ?? start.capturedAtRaw ?? existing?.startTripTime ?? "",
        endTripTime: end.bestTime ?? end.capturedAtRaw ?? existing?.endTripTime ?? "",
        timezone: value.timezone,
        originTimezone: value.originTimezone,
        destinationTimezone: value.destinationTimezone,
        displayMode: value.kind === "location" ? "local" : value.displayMode,
      };
      if (phaseDialog.split) {
        await chronologyApi.splitTripPhase(
          projectId,
          phaseDialog.split.existingPhaseId,
          phaseDialog.split.boundaryAssetId,
          phaseDialog.split.direction,
          draft,
        );
      } else {
        await chronologyApi.setTripPhase(projectId, draft);
      }
      await session.reloadChronology();
      setPhaseDialog(null);
      setMessage(existing
        ? `Updated ${value.label}`
        : phaseDialog.split
          ? `Split the ribbon with ${value.label}`
          : `Added ${value.label} to the story ribbon`);
    } catch (reason) {
      setPhaseDialogError(String(reason));
    } finally {
      setPhaseDialogBusy(false);
    }
  }

  async function removeTripPhase() {
    const phase = phaseDialog?.phase;
    if (!phase || !projectId) return;
    setPhaseDialogBusy(true);
    setPhaseDialogError("");
    try {
      await chronologyApi.removeTripPhase(projectId, phase.id);
      await session.reloadChronology();
      setPhaseDialog(null);
      setMessage(`Removed ${phase.label} from the story ribbon`);
    } catch (reason) {
      setPhaseDialogError(String(reason));
    } finally {
      setPhaseDialogBusy(false);
    }
  }

  const handleNotesKeyDown = (event: ReactKeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();

      // In Focus the caret starts in this field, so Escape has to be the way
      // out or the round trip costs two keys and a discarded paragraph. It
      // saves and leaves. In Review it keeps its cancel meaning: there you
      // reached the field deliberately and backing out should undo.
      if (view.presentation === "focus") {
        notesEditingRef.current = null;
        event.currentTarget.blur();
        if (selected) void persistNotes(selected);
        dispatchView({ kind: "exitFocus" });
        return;
      }

      const editing = notesEditingRef.current;
      if (selected && editing?.assetId === selected.id) {
        editing.cancelOnBlur = true;
        replaceAsset({ ...selected, notes: editing.savedNotes });
      }
      event.currentTarget.blur();
      return;
    }

    if (event.key !== "Enter" || event.nativeEvent.isComposing) return;

    // Enter inserts a newline; ⌘Enter saves; ⇧Enter saves and advances.
    // This inverts the old mapping deliberately. Enter-to-save is hostile in
    // a field sized for 18 lines of prose, and ⌘Enter-for-newline inverts the
    // macOS convention every other text field on the machine follows.
    if (!event.metaKey && !event.shiftKey) return;

    event.preventDefault();
    event.stopPropagation();
    if (event.repeat || !selected) return;

    notesEditingRef.current = null;
    event.currentTarget.blur();
    void persistNotes(selected);

    if (event.shiftKey) {
      moveSelection(1);
    }
  };

  const handleNotesBlur = () => {
    const editing = notesEditingRef.current;
    notesEditingRef.current = null;
    if (!selected || editing?.assetId !== selected.id || editing.cancelOnBlur) return;
    void persistNotes(selected);
  };

  function closePairingWizard() {
    setShowPairingWizard(false);
    setPairingWizardStep("endpoint");
    setPairingCode("");
    setServerIdentity(null);
  }

  /**
   * Every surface Escape can close, top of the stack first (issue #52).
   *
   * Only settings→pairing is a stack the user can really build, but the order
   * is written out anyway: two surfaces on screen at once must have a defined
   * winner, and "whichever branch of an if-chain ran first" is not one.
   *
   * The chronology desk's pending-preview dock resolves its own Escape on
   * `document` and stops propagation, so it wins over this list without being
   * in it — a transient inside a mode, not a surface over the shell.
   */
  const escapeLayers: EscapeLayer[] = [
    { id: "cameraMerge", open: Boolean(cameraMergeSource), locked: cameraMergeBusy },
    { id: "tripPhase", open: Boolean(phaseDialog), locked: phaseDialogBusy },
    // The dialog takes itself off screen when the run starts, so this lock
    // mirrors `closeImportDialog`'s own guard rather than covering a state the
    // interface can currently reach. A preflight still in progress is *not*
    // locked: closing it cancels the preflight, which is free.
    { id: "import", open: showImportDialog, locked: importRunning },
    { id: "importHistory", open: showImportHistory },
    { id: "baseline", open: showBaselineWizard, locked: baselineStep === "confirming" },
    { id: "pairing", open: showPairingWizard, locked: busy },
    { id: "sync", open: showSyncDiagnostics },
    { id: "analysis", open: showAnalysisDialog },
    { id: "shortcuts", open: showShortcuts },
    { id: "settings", open: showSettings },
    // With no project open there is nothing behind this modal to go back to.
    { id: "create", open: showCreate, locked: projects.length === 0 },
    { id: "mediaMenu", open: showMediaMenu },
    { id: "focus", open: view.presentation === "focus" },
    { id: "mode", open: view.mode !== "review" },
  ];

  function closeEscapeLayer(id: string) {
    switch (id) {
      case "cameraMerge": return closeCameraMerge();
      case "tripPhase": return setPhaseDialog(null);
      case "import": return closeImportDialog();
      case "importHistory": return (setShowImportHistory(false), setImportHistoryError(""));
      case "baseline": return handleCancelBaselineWizard();
      case "pairing": return closePairingWizard();
      case "sync": return setShowSyncDiagnostics(false);
      case "analysis": return setShowAnalysisDialog(false);
      case "shortcuts": return setShowShortcuts(false);
      case "settings": return setShowSettings(false);
      case "create": return setShowCreate(false);
      case "mediaMenu": return setShowMediaMenu(false);
      case "focus": return dispatchView({ kind: "exitFocus" });
      case "mode": return returnToReview();
    }
  }

  // Read through a ref so the key handler always resolves against this render's
  // layers. The handler's effect deps do not list every modal flag, and a
  // stale layer list is exactly the class of bug this change removes.
  const escapeRef = useRef({ layers: escapeLayers, close: closeEscapeLayer });
  escapeRef.current = { layers: escapeLayers, close: closeEscapeLayer };

  useEffect(() => {
    const handleKey = (event: KeyboardEvent) => {
      const key = event.key.toLocaleLowerCase();
      if (event.metaKey && key === "n") {
        event.preventDefault();
        setShowCreate(true);
        return;
      }
      if (event.metaKey && key === "o") {
        event.preventDefault();
        void openCatalog();
        return;
      }
      if (event.metaKey && event.key === ",") {
        event.preventDefault();
        setShowSettings(true);
        return;
      }
      if (event.key === "Escape") {
        // One owner for "what is on top" (issue #52). Every surface that can
        // be on screen is in `escapeLayers`, in stack order, and exactly one
        // of them answers.
        const { layers, close } = escapeRef.current;
        const resolution = resolveEscape(layers);
        if (resolution.kind !== "none") event.preventDefault();
        if (resolution.kind === "close") close(resolution.id);
        return;
      }
      const tag = (event.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;

      const reviewAction = resolveAction({
        key: event.key,
        metaKey: event.metaKey,
        ctrlKey: event.ctrlKey,
        altKey: event.altKey,
        shiftKey: event.shiftKey,
        target:
          event.target instanceof HTMLElement
            ? { tagName: event.target.tagName, isContentEditable: event.target.isContentEditable }
            : null,
      });
      if (reviewAction) {
        switch (reviewAction.kind) {
          case "move":
            event.preventDefault();
            moveSelection(reviewAction.delta);
            return;
          case "toggleRemoved":
            if (selected) toggleRemoved(selected);
            return;
          case "zoom":
            event.preventDefault();
            setChronologyZoom((current: number) =>
              reviewAction.delta < 0
                ? Math.max(RIBBON_ZOOM_MIN, current - RIBBON_ZOOM_STEP)
                : Math.min(RIBBON_ZOOM_MAX, current + RIBBON_ZOOM_STEP),
            );
            return;
          case "centerSelection":
            event.preventDefault();
            centerSelectedInChronology();
            return;
          case "jumpBestMoment": {
            const startMs = observation?.analysis.bestMomentStartMs;
            if (startMs === null || startMs === undefined) return;
            event.preventDefault();
            seekTo(startMs / 1000);
            return;
          }
          case "editTitle":
            event.preventDefault();
            document.querySelector<HTMLInputElement>(".clip-title-field")?.focus();
            return;
          case "rotate":
            if (!selected) return;
            event.preventDefault();
            void persistAsset(selected, {
              rotationDegrees: rotationAfter(selected.rotationDegrees, reviewAction.addition),
            });
            return;
          case "setMode":
            event.preventDefault();
            if (reviewAction.mode === "review" || projectId) {
              if (reviewAction.mode === "chronology") openChronology();
              else if (reviewAction.mode === "review") returnToReview();
              else dispatchView({ kind: "setMode", mode: reviewAction.mode });
            }
            return;
          case "toggleRibbon":
            event.preventDefault();
            dispatchView({ kind: "toggleRibbon" });
            return;
          case "toggleShelf":
            // D means "the machine-output surface" in both presentations: the
            // evidence shelf in Review, the right stack in Focus. One key,
            // one meaning, learned once.
            event.preventDefault();
            dispatchView(
              view.presentation === "focus"
                ? { kind: "toggleRightStack" }
                : { kind: "toggleShelf" },
            );
            return;
          case "toggleFocus":
            event.preventDefault();
            dispatchView(
              view.presentation === "focus"
                ? { kind: "exitFocus" }
                : { kind: "enterFocus", overlays: true },
            );
            return;
          case "toggleOverlays":
            event.preventDefault();
            dispatchView({ kind: "toggleOverlays" });
            return;
          case "ribbonOverlay":
            event.preventDefault();
            if (view.mode === "chronology") returnToReview();
            else openChronology();
            return;
        }
      }

      if (event.key === "?") {
        event.preventDefault();
        setShowShortcuts(true);
      } else if (event.key === "/") {
        event.preventDefault();
        document.querySelector<HTMLInputElement>("#queue-search")?.focus();
      } else if (event.code === "Space") {
        event.preventDefault();
        if (videoRef.current?.paused) void videoRef.current.play();
        else videoRef.current?.pause();
      } else if (selected && key === "l") {
        event.preventDefault();
        document.querySelector<HTMLInputElement>("#label-input")?.focus();
      } else if (key === "n" || (key === "enter" && tag !== "BUTTON" && !event.shiftKey && !event.metaKey && !event.ctrlKey && !event.altKey)) {
        event.preventDefault();
        document.querySelector<HTMLTextAreaElement>("#notes")?.focus();
      } else if (key === "p" && event.shiftKey) {
        void makeMissingProxies();
      } else if (key === "p") {
        void makeProxy();
      } else if (key === "g" && event.shiftKey) {
        // Thumbnails gave up bare T to the ribbon toggle: generating them is a
        // once-per-project setup action, and T is pressed many times a session.
        void makeMissingThumbnails();
      } else if (key === "a") {
        void openAnalysisDialog();
      } else if (key === "m") {
        void importFolder();
      }
    };
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [busy, centerSelectedInChronology, moveSelection, observation, openChronology, persistAsset, projectId, projects.length, returnToReview, selected, showAnalysisDialog, showMediaMenu, showSettings, showShortcuts, toggleRemoved, view.mode, view.presentation]);

  async function createProject() {
    const name = newProjectName.trim();
    if (!name) return;
    setBusy(true);
    setError("");
    try {
      const created = await api.createProject(name);
      await Promise.all([refreshProjects(), refreshLibrary()]);
      setProjectId(created.id);
      setShowCreate(false);
      setNewProjectName("");
      setMessage("Project created");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  async function openLibraryProject(location: ProjectLocation) {
    setBusy(true);
    setError("");
    setMessage(`Opening ${location.project.name}…`);
    try {
      const loaded = await api.loadCatalog(location.catalogPath);
      setProjects(loaded.projects);
      setProjectId(location.project.id);
      setSelectedId("");
      setShowCreate(false);
      setMessage(`Loaded ${location.project.name}`);
    } catch (reason) {
      setError(String(reason));
      setMessage("Could not open project");
    } finally {
      setBusy(false);
    }
  }

  async function configureProjectsRoot() {
    setError("");
    try {
      const path = await api.chooseProjectsRoot();
      if (!path) return;
      setBusy(true);
      const result = await api.setProjectsRoot(path);
      setLibrary(result);
      setSettings((current) => ({ ...current, projectsRoot: result.rootPath }));
      setMessage(`Default project folder: ${result.rootPath}`);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  async function updateAutoplayMode(autoplayMode: AutoplayMode) {
    const previous = settings;
    setSettings({ ...settings, autoplayMode });
    setError("");
    try {
      setSettings(await api.setAutoplayMode(autoplayMode));
      setMessage("Playback settings saved");
    } catch (reason) {
      setSettings(previous);
      setError(String(reason));
    }
  }

  async function openCatalog() {
    setError("");
    try {
      const path = await api.chooseCatalog();
      if (!path) return;
      setBusy(true);
      setMessage("Opening catalog…");
      const loaded = await api.loadCatalog(path);
      setProjects(loaded.projects);
      setProjectId(loaded.projects[0]?.id ?? "");
      setSelectedId("");
      setShowCreate(loaded.projects.length === 0);
      setMessage(
        loaded.projects.length
          ? `Loaded ${loaded.projects.length} project${loaded.projects.length === 1 ? "" : "s"} from ${loaded.path}`
          : `Catalog ${loaded.path} is empty; create its first project`,
      );
    } catch (reason) {
      setError(String(reason));
      setMessage("Could not open catalog");
    } finally {
      setBusy(false);
    }
  }

  async function importFolder() {
    if (!projectId) return;
    setError("");
    const path = await api.chooseFolder();
    if (!path) return;
    await openImportForPath(path);
  }

  /**
   * `resume` carries an interrupted session's own choices back in (#67), so
   * reconnecting a card does not silently re-derive a destination or drop a
   * camera the user already chose. Everything else about the reopened dialog
   * is a fresh classification of whatever is actually there now.
   */
  async function openImportForPath(
    path: string,
    knownCard?: CardCandidate,
    resume?: { archiveRootId: string; cameraOverride: string | null },
  ) {
    setShowImportDialog(true);
    setImportDisconnected(false);
    setImportProjectName(project?.name ?? "this project");
    setImportSource(path);
    setImportPlan(null);
    setImportProgress(null);
    setImportReport(null);
    setImportError("");
    setImportCameraOverride(resume?.cameraOverride ?? null);
    setImportKnownCameras([]);
    setImportCameraMappings([]);
    setImportLoading(true);
    // Held outside the try so a preflight that dies on a vanished card can
    // still record which destination the run would have used.
    let resolvedCard: CardCandidate | null = knownCard ?? null;
    let resolvedRootId = resume?.archiveRootId ?? "";
    try {
      const [card, roots, knownCameras, mappings] = await Promise.all([
        knownCard ? Promise.resolve(knownCard) : api.inspectImportSource(path),
        api.listArchiveRoots(),
        projectId ? api.knownProjectCameras(projectId) : Promise.resolve([]),
        projectId ? api.listCameraMappings(projectId) : Promise.resolve([]),
      ]);
      resolvedCard = card;
      setImportCard(card);
      setArchiveRoots(roots);
      setImportKnownCameras(knownCameras);
      setImportCameraMappings(mappings);
      const mappedRootId = card
        ? settings.importArchiveRootIdsByCardSession[card.sessionId]
        : null;
      const resumedRoot = resume ? roots.find((root) => root.id === resume.archiveRootId) : undefined;
      const remembered = resumedRoot
        ?? roots.find((root) => root.id === mappedRootId)
        ?? roots.find((root) => root.id === settings.lastImportArchiveRootId);
      const selectedRoot = card ? (remembered ?? roots.find((root) => root.available) ?? roots[0] ?? null) : null;
      const rootId = selectedRoot?.id ?? "";
      resolvedRootId = rootId;
      setSelectedArchiveRootId(rootId);
      if (!card || rootId) {
        setImportPlan(
          await api.preflightImport(projectId, path, rootId || null, resume?.cameraOverride ?? null),
        );
      }
    } catch (reason) {
      if (String(reason).includes("preflight cancelled")) return;
      // Classifying a card is the first thing that touches it, so it is also
      // the first thing a loose cable breaks. Reviewing a card dismisses its
      // banner, so without this the card could come back and have nothing
      // left to offer the user — the recovery trap #67 names.
      const session: InterruptedImportOptions = {
        projectId,
        sourcePath: path,
        card: resolvedCard,
        archiveRootId: resolvedRootId,
        cameraOverride: resume?.cameraOverride ?? null,
        verifyCopies: verifyImportCopies,
        makeProxies: makeImportProxies,
      };
      if (await sourceWentAway(session)) {
        setInterruptedImport({ ...session, committed: 0, failed: 0, interruptedAt: Date.now() });
        setImportDisconnected(true);
        return;
      }
      setImportError(String(reason));
    } finally {
      setImportLoading(false);
    }
  }

  async function refreshImportPlan(rootId: string, cameraOverride: string | null = importCameraOverride) {
    if (!projectId || !importSource || (importCard && !rootId)) {
      setImportPlan(null);
      return;
    }
    setSelectedArchiveRootId(rootId);
    setImportLoading(true);
    setImportError("");
    setImportProgress(null);
    try {
      setImportPlan(await api.preflightImport(projectId, importSource, rootId || null, cameraOverride));
    } catch (reason) {
      // Never leave the previous camera plan actionable after a failed replan.
      // It may carry an override or remembered mapping the user just changed.
      setImportPlan(null);
      if (!String(reason).includes("preflight cancelled")) setImportError(String(reason));
    } finally {
      setImportLoading(false);
    }
  }

  /**
   * The user picked an existing project camera, typed a deliberate new name,
   * or cleared back to auto-detection (#71). Re-runs preflight with the
   * override so the sequence preview reflects it before Import is pressed —
   * the same identity `run_import` will then use for every new asset, so the
   * two can never disagree (see `preflight.rs`'s `ResolvedImportCamera` doc).
   */
  async function selectImportCamera(camera: string | null) {
    setImportCameraOverride(camera);
    await refreshImportPlan(selectedArchiveRootId, camera);
  }

  /** The "change" half of inspect/change/clear (#71). Re-plans afterward: a
   * rename can change what the currently mounted card resolves to. */
  async function renameImportCameraMapping(mappingId: string, camera: string) {
    if (!projectId) return;
    try {
      const updated = await api.renameCameraMapping(projectId, mappingId, camera);
      setImportCameraMappings((current) => current.map((mapping) => (mapping.id === updated.id ? updated : mapping)));
      await refreshImportPlan(selectedArchiveRootId);
    } catch (reason) {
      setImportError(String(reason));
    }
  }

  /** The "clear" half of inspect/change/clear (#71). */
  async function clearImportCameraMapping(mappingId: string) {
    if (!projectId) return;
    try {
      await api.clearCameraMapping(projectId, mappingId);
      setImportCameraMappings((current) => current.filter((mapping) => mapping.id !== mappingId));
      await refreshImportPlan(selectedArchiveRootId);
    } catch (reason) {
      setImportError(String(reason));
    }
  }

  async function createImportArchive(name: string) {
    const path = await api.chooseArchiveFolder(`Where should “${name}” live?`);
    if (!path) return;
    setImportLoading(true);
    try {
      const root = await api.createArchiveRoot(name, path);
      setArchiveRoots((current) => [root, ...current.filter((item) => item.id !== root.id)]);
      await refreshImportPlan(root.id);
    } catch (reason) {
      setImportError(String(reason));
      setImportLoading(false);
    }
  }

  async function locateImportArchive(root: ArchiveRoot) {
    const path = await api.chooseArchiveFolder(`Where is “${root.displayName}” mounted now?`);
    if (!path) return;
    setImportLoading(true);
    try {
      const rebound = await api.bindArchiveRoot(root.id, path);
      setArchiveRoots((current) => current.map((item) => item.id === rebound.id ? rebound : item));
      await refreshImportPlan(rebound.id);
    } catch (reason) {
      setImportError(String(reason));
      setImportLoading(false);
    }
  }

  async function runSmartImport() {
    if (!projectId || !importPlan || importRunning || busy) return;
    const importingProjectId = projectId;
    // Snapshot the choices now, while the dialog still holds them: a source
    // that disappears mid-run has to be resumable with the same destination,
    // camera, and verification the user agreed to (#67).
    const sessionOptions: InterruptedImportOptions = {
      projectId: importingProjectId,
      sourcePath: importSource,
      card: importCard,
      archiveRootId: selectedArchiveRootId,
      cameraOverride: importCameraOverride,
      verifyCopies: verifyImportCopies,
      makeProxies: makeImportProxies,
    };
    setImportRunning(true);
    setBusy(true);
    setImportProgress(null);
    setImportError("");
    activeOperationRef.current = "import";
    setOperationProgress(initialProgress("import", importPlan.newClips + importPlan.relinkable));
    setShowImportDialog(false);
    setImportDisconnected(false);
    try {
      const nextSettings = await api.setImportPreferences(
        makeImportProxies,
        selectedArchiveRootId || null,
        importCard?.sessionId ?? null,
      );
      setSettings(nextSettings);
      const report = await api.runSmartImport(importingProjectId, importPlan, verifyImportCopies, makeImportProxies);
      // The worker may finish after the user has opened another project. Its
      // durable work remains valid, but its ledger, message, and reloads must
      // not overwrite the project now on screen.
      if (projectIdRef.current !== importingProjectId) return;
      // The incremental events already made committed rows visible. This final
      // pass reconciles anything an event could not carry, including all
      // solver-derived chronology after the last card run landed.
      await Promise.all([
        reloadSessionAssets(),
        reconcileImportChronology(importingProjectId),
        refreshProjects(),
        session.reloadReadiness(),
      ]);
      // Reconciliation itself is asynchronous. The user can leave while it is
      // running, so publish the ledger and completion message only after the
      // same project guard has held across that final await too.
      if (projectIdRef.current !== importingProjectId) return;
      setImportSessions((current) => report.session ? [report.session, ...current.filter((entry) => entry.id !== report.session!.id)] : current);
      // A source that went away is a recoverable state, not a result. Record
      // the session, reopen the dialog on its waiting screen, and leave the
      // completed-run report unset — there is no completed run to report.
      const interruption = interruptionFromReport(sessionOptions, report);
      setInterruptedImport((current) => {
        if (interruption) return interruption;
        // A run that finished on its own terms clears only its own session's
        // recovery state; an unrelated card still waiting stays waiting.
        return current && isSameInterruptedSession(current, sessionOptions) ? null : current;
      });
      if (interruption) {
        setImportDisconnected(true);
        setShowImportDialog(true);
        setMessage(interruptedSourceHeadline(interruption));
        return;
      }
      setImportReport(report);
      const failures = report.outcome.failures.length + (report.proxies?.failures.length ?? 0);
      const warnings = report.outcome.warnings?.length ?? 0;
      if (failures) {
        const failureMessage = `${failures} file${failures === 1 ? "" : "s"} need attention in the import ledger.`;
        setImportError(failureMessage);
        setError(failureMessage);
      } else if (report.outcome.cancelled || report.proxies?.cancelled) {
        setMessage("Import stopped cleanly; finished files were kept");
      } else if (warnings) {
        setMessage(
          `Import complete · ${report.outcome.imported} clip${report.outcome.imported === 1 ? "" : "s"} added · ${warnings} warning${warnings === 1 ? "" : "s"} to review`,
        );
      } else {
        setMessage(`Import complete · ${report.outcome.imported} clip${report.outcome.imported === 1 ? "" : "s"} added`);
      }
    } catch (reason) {
      if (projectIdRef.current !== importingProjectId) return;
      // The run can also fail outright — a destination that bails, an error
      // raised before the per-item loop could classify anything. Ask the
      // hardware directly before settling for a generic error: if the card
      // this run was reading is no longer the card in the slot, the honest
      // reading is an interruption with a way back, not a dead end (#67).
      if (await sourceWentAway(sessionOptions)) {
        const interruption: InterruptedImport = {
          ...sessionOptions,
          committed: 0,
          failed: 0,
          interruptedAt: Date.now(),
        };
        setInterruptedImport(interruption);
        setImportDisconnected(true);
        setShowImportDialog(true);
        setMessage(interruptedSourceHeadline(interruption));
        return;
      }
      setError(String(reason));
    } finally {
      activeOperationRef.current = null;
      setOperationProgress(null);
      setImportRunning(false);
      setBusy(false);
    }
  }

  /**
   * Whether the card this run was reading has left. Answered by identity, so
   * a different card already remounted at the same path counts as gone.
   * Only card sources can be answered this way; a plain folder has no identity
   * to compare and keeps the ordinary error path.
   */
  async function sourceWentAway(session: InterruptedImportOptions): Promise<boolean> {
    if (!session.card) return false;
    try {
      return !isSameCardSession(session.card, await api.inspectImportSource(session.sourcePath));
    } catch {
      return true;
    }
  }

  /**
   * Reopen the interrupted session's dialog (#67). Called from the media menu,
   * because a modal the user closed — or one that was never reopened — must
   * not be the only way back to an import that is still waiting on hardware.
   */
  function reopenInterruptedImport() {
    if (!interruptedImport) return;
    setShowMediaMenu(false);
    if (resumableCard) {
      void resumeInterruptedImport(resumableCard);
      return;
    }
    setImportProjectName(project?.name ?? "this project");
    setImportSource(interruptedImport.sourcePath);
    setImportCard(interruptedImport.card);
    // The plan described a card that is no longer in the slot; a resume
    // re-classifies from scratch rather than reusing it.
    setImportPlan(null);
    setImportError("");
    setImportProgress(null);
    setImportReport(null);
    setImportDisconnected(true);
    setShowImportDialog(true);
  }

  /**
   * Resume into a source that has been verified as this session's own. The
   * identity check is repeated here rather than trusted from the poll: the
   * only thing standing between "the card came back" and "a different card is
   * at that path" is this comparison.
   */
  async function resumeInterruptedImport(card: CardCandidate | null) {
    const session = interruptedImport;
    if (!session || !projectId || session.projectId !== projectId) return;
    if (session.card && !matchesInterruptedSource(session, card)) return;
    setVerifyImportCopies(session.verifyCopies);
    setMakeImportProxies(session.makeProxies);
    setShowMediaMenu(false);
    await openImportForPath(card?.mountPath ?? session.sourcePath, card ?? undefined, {
      archiveRootId: session.archiveRootId,
      cameraOverride: session.cameraOverride,
    });
  }

  /**
   * "Choose source…" from the waiting state. A source the user points at by
   * hand is an explicit decision, not an inference from a pathname — but it
   * only *resumes* the interrupted session when it is that session's card.
   * Anything else opens as the ordinary new import it is, and leaves the
   * interrupted session still waiting in the media menu.
   */
  async function chooseInterruptedImportSource() {
    const path = await api.chooseFolder();
    if (!path) return;
    const card = await api.inspectImportSource(path).catch(() => null);
    if (interruptedImport && matchesInterruptedSource(interruptedImport, card)) {
      await resumeInterruptedImport(card);
      return;
    }
    setImportDisconnected(false);
    await openImportForPath(path, card ?? undefined);
  }

  /**
   * "Everything before this clip is old" (#66). Stored against the card
   * session, so it survives this dialog, this app run, and this mount point.
   *
   * Re-runs preflight rather than filtering the plan on screen: the boundary
   * changes the byte cost, the camera evidence, and the sequence proposals, and
   * a dialog whose summary disagreed with what import would actually do would
   * be worse than no cutoff at all.
   */
  async function setImportCutoff(item: PreflightItem) {
    if (!projectId || !importCard) return;
    setImportError("");
    try {
      await api.setImportCutoff(
        projectId,
        importCard.sessionId,
        importCard.label,
        item.relativePath,
        item.filename,
        item.filesystemModifiedAt ?? null,
      );
    } catch (reason) {
      setImportError(String(reason));
      return;
    }
    await refreshImportPlan(selectedArchiveRootId);
  }

  /** Forget the boundary, offering the whole card again. */
  async function clearImportCutoff() {
    if (!projectId || !importCard) return;
    setImportError("");
    try {
      await api.clearImportCutoff(projectId, importCard.sessionId);
    } catch (reason) {
      setImportError(String(reason));
      return;
    }
    await refreshImportPlan(selectedArchiveRootId);
  }

  /** Give up on the interrupted session. Safe and idempotent from any state. */
  function cancelInterruptedImport() {
    // Back to before the import: the card is offerable again, because the
    // dismissal that suppressed its banner was part of the run being abandoned.
    if (interruptedImport?.card) dismissedCardSessionsRef.current.delete(interruptedImport.card.sessionId);
    setInterruptedImport(null);
    setResumableCard(null);
    setImportDisconnected(false);
    setShowImportDialog(false);
    setImportError("");
    setMessage("Interrupted import cancelled; every committed clip was kept");
  }

  function reviewMountedCard() {
    if (!mountedCard) return;
    dismissedCardSessionsRef.current.add(mountedCard.sessionId);
    const card = mountedCard;
    setMountedCard(null);
    void openImportForPath(card.mountPath, card);
  }

  function dismissMountedCard() {
    if (mountedCard) dismissedCardSessionsRef.current.add(mountedCard.sessionId);
    setMountedCard(null);
  }

  function closeImportDialog() {
    if (importRunning) return;
    if (importLoading) void api.cancelImport();
    // Closing the waiting screen puts the interrupted session away; it does
    // not abandon it. The media menu is how it comes back (#67).
    setImportDisconnected(false);
    setShowImportDialog(false);
  }

  async function openImportHistory() {
    if (!projectId) return;
    setShowMediaMenu(false);
    setImportSessionDetail(null);
    setImportRollback(null);
    setImportHistoryError("");
    try {
      setImportSessions(await api.listImportSessions(projectId));
    } catch (reason) {
      // The dialog never opens, so this one belongs on the main banner: the
      // alternative is a click that visibly does nothing.
      setError(String(reason));
      return;
    }
    setShowImportHistory(true);
  }

  async function openImportSession(sessionId: string) {
    if (!projectId) return;
    setImportRollback(null);
    setImportHistoryError("");
    try {
      setImportSessionDetail(await api.importSession(projectId, sessionId));
    } catch (reason) {
      setImportHistoryError(String(reason));
    }
  }

  async function previewImportRollback(sessionId: string) {
    if (!projectId) return;
    setImportHistoryError("");
    try {
      setImportRollback(await api.importRollbackPreview(projectId, sessionId));
    } catch (reason) {
      setImportHistoryError(String(reason));
    }
  }

  async function rollbackImportSession(sessionId: string) {
    if (!projectId || busy) return;
    setBusy(true);
    setImportHistoryError("");
    try {
      await api.rollbackImportMembership(projectId, sessionId);
      await Promise.all([reloadSessionAssets(), api.listImportSessions(projectId).then(setImportSessions)]);
      setImportRollback(null);
      setImportSessionDetail(await api.importSession(projectId, sessionId));
      setMessage("Import membership removed; original media was left untouched");
    } catch (reason) {
      // The confirmation stays armed: a rollback that failed is one the user
      // still means to make, and silence here reads as "it worked".
      setImportHistoryError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  /**
   * Reconnect this clip's originals, from the clip that is missing them.
   *
   * The same project-wide relink the chronology desk's offline-volume
   * diagnostic runs: a folder search repairs every asset it recognises, not
   * only the selected one, because a volume comes back all at once.
   */
  async function locateOriginals(asset: Asset) {
    if (!projectId || busy) return;
    const folder = await api.chooseRelinkFolder(sourceVolumeHint(asset));
    if (!folder) return;
    setBusy(true);
    setError("");
    try {
      const report = await api.relinkFolder(projectId, folder);
      // The queue's offline marks come from the assets, so re-reading them is
      // what clears the state without a relaunch.
      await reloadSessionAssets();
      setMessage(report.repaired
        ? `Reconnected ${report.repaired} clip${report.repaired === 1 ? "" : "s"}`
        : "Nothing in that folder matched this project's offline clips");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  /**
   * Enter in the search box: put the playhead on the next place the phrase was
   * said in the selected clip (issue #12).
   *
   * Repeated presses cycle, because "say that again" is why anyone presses it
   * twice and a control that goes dead on the last match makes the reader work
   * out which one they are on.
   */
  function jumpToSpokenMatch() {
    if (!selected || !query.trim()) return;
    const segments = transcript?.segments ?? [];
    const matches = transcriptMatches(segments, query);
    if (!matches.length) {
      // The clip is in the results, so it matched something; saying which is
      // more use than saying nothing.
      setMessage("Nothing spoken here matches — this clip matched elsewhere");
      return;
    }
    const target = nextMatch(matches, spokenMatchRef.current);
    if (target === null) return;
    spokenMatchRef.current = target;
    setShelfTab("transcript");
    if (!view.shelf) dispatchView({ kind: "toggleShelf" });
    seekTo(segments[target].startMs / 1000);
    setMessage(`Spoken match ${matches.indexOf(target) + 1} of ${matches.length}`);
  }

  async function makeProxy() {
    if (!selected || busy) return;
    setBusy(true);
    activeOperationRef.current = "proxy";
    setOperationProgress(initialProgress("proxy", 1));
    setMessage(`Making a 720p proxy for ${selected.filename}…`);
    setError("");
    try {
      replaceAsset(await api.generateProxy(selected.id));
      setMessage("Proxy ready for offline review");
    } catch (reason) {
      setError(String(reason));
    } finally {
      activeOperationRef.current = null;
      setOperationProgress(null);
      setBusy(false);
    }
  }

  async function makeMissingProxies() {
    if (!projectId || busy) return;
    setBusy(true);
    activeOperationRef.current = "proxy";
    setOperationProgress(initialProgress("proxy", missingProxyCount));
    setMessage("Making missing laptop proxies…");
    setError("");
    try {
      const report = await api.generateMissingProxies(projectId);
      await reloadSessionAssets();
      setMessage(
        `Made ${report.generated} proxies; ${report.skipped} already ready${
          report.failures.length ? `; ${report.failures.length} failed` : ""
        }`,
      );
      if (report.failures.length) setError(report.failures[0].error);
    } catch (reason) {
      setError(String(reason));
    } finally {
      activeOperationRef.current = null;
      setOperationProgress(null);
      setBusy(false);
    }
  }

  async function openAnalysisDialog() {
    if (!projectId || busy) return;
    setShowMediaMenu(false);
    setError("");
    try {
      await Promise.all([session.reloadReadiness(), api.modelStatus().then(setModels), api.openRouterKeyPresent().then(setKeyPresent)]);
      setShowAnalysisDialog(true);
    } catch (reason) {
      setError(String(reason));
    }
  }

  async function runAnalysis() {
    if (!projectId || busy) return;
    const budget = Number.parseFloat(budgetInput);
    setShowAnalysisDialog(false);
    setBusy(true);
    activeOperationRef.current = "analysis";
    setOperationProgress({ ...initialProgress("analysis", readiness?.totalClips ?? 0), stage: "proxies", stageIndex: 0, stageCount: 3, costUsd: 0 });
    setMessage("Analyzing project…");
    setError("");
    try {
      const report = await api.analyzeProject(projectId, {
        force: forceReanalyze,
        budgetUsd: Number.isFinite(budget) && budget > 0 ? budget : null,
      });
      // Everything the run touched, including the evidence for the clip the
      // user is sitting on: the queue row's verdict and the rail beside it now
      // come from the same read.
      await reloadSession();
      const spent = formatCost(report.costUsd);
      if (report.status === "cancelled") {
        setMessage(`Analysis stopped during ${analysisStageLabels[report.stageReached].toLocaleLowerCase()}; ${report.analysis.analyzed} clips analyzed, ${spent} spent. Run it again to resume.`);
      } else {
        setMessage(`Analyzed ${report.analysis.analyzed} clips and transcribed ${report.transcripts.transcribed}; ${spent} spent`);
      }
      if (report.analysis.budgetExhausted) {
        setError(`Stopped at the ${formatCost(budget)} budget for this run. Raise it and run again to continue.`);
      }
      const failure = report.proxies.failures[0] ?? report.transcripts.failures[0] ?? report.analysis.failures[0];
      if (failure && !report.analysis.budgetExhausted) setError(failure.error);
    } catch (reason) {
      setError(String(reason));
    } finally {
      activeOperationRef.current = null;
      setOperationProgress(null);
      setBusy(false);
    }
  }

  async function stopAnalysis() {
    try {
      await api.cancelAnalysis();
      setMessage("Stopping after the clip in progress…");
    } catch (reason) {
      setError(String(reason));
    }
  }

  async function installModels() {
    setError("");
    setModelInstall({ name: "", downloadedBytes: 0, totalBytes: 1 });
    try {
      setModels(await api.installAnalysisModels());
      if (projectId) await session.reloadReadiness();
      setMessage("Speech models installed");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setModelInstall(null);
    }
  }

  async function saveOpenRouterKey() {
    setError("");
    try {
      setKeyPresent(await api.setOpenRouterKey(keyInput));
      setKeyInput("");
      if (projectId) await session.reloadReadiness();
      setMessage(keyPresent ? "OpenRouter key updated" : "OpenRouter key stored in your Keychain");
    } catch (reason) {
      setError(String(reason));
    }
  }

  function seekTo(seconds: number) {
    const video = videoRef.current;
    if (!video) return;
    video.currentTime = Math.max(0, seconds);
    setPlaybackSeconds(video.currentTime);
  }

  // The lane gutter corrects a camera guess for every clip that shares it, so
  // the whole queue is refreshed rather than a single asset replaced.
  async function renameCamera(from: string, to: string) {
    if (!projectId || busy) return;
    setBusy(true);
    setError("");
    try {
      const moved = await api.renameCamera(projectId, from, to);
      await Promise.all([reloadSessionAssets(), session.reloadChronology()]);
      setMessage(`Renamed ${from} to ${to} on ${moved} ${moved === 1 ? "clip" : "clips"}`);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  function openCameraMerge(sourceCamera: string) {
    cameraMergeRequestRef.current += 1;
    setCameraMergeSource(sourceCamera);
    setCameraMergeDestination("");
    setCameraMergePreview(null);
    setCameraMergeError("");
  }

  function restoreCameraMergeFocus(sourceCamera: string) {
    queueMicrotask(() => {
      const trigger = [...document.querySelectorAll<HTMLButtonElement>(".lane-action-button")]
        .find((button) => button.getAttribute("aria-label") === `Camera actions for ${sourceCamera}`);
      trigger?.focus();
    });
  }

  function closeCameraMerge() {
    if (cameraMergeBusy) return;
    const sourceCamera = cameraMergeSource;
    cameraMergeRequestRef.current += 1;
    setCameraMergeSource("");
    setCameraMergeDestination("");
    setCameraMergePreview(null);
    setCameraMergeError("");
    restoreCameraMergeFocus(sourceCamera);
  }

  async function selectCameraMergeDestination(destinationCamera: string) {
    setCameraMergeDestination(destinationCamera);
    setCameraMergePreview(null);
    setCameraMergeError("");
    const request = ++cameraMergeRequestRef.current;
    if (!projectId || !cameraMergeSource || !destinationCamera) return;
    try {
      const preview = await api.previewCameraMerge(projectId, cameraMergeSource, destinationCamera);
      if (request === cameraMergeRequestRef.current && preview.projectId === projectId) setCameraMergePreview(preview);
    } catch (reason) {
      if (request === cameraMergeRequestRef.current) setCameraMergeError(String(reason));
    }
  }

  async function confirmCameraMerge() {
    if (!projectId || !cameraMergePreview || cameraMergePreview.projectId !== projectId || cameraMergeBusy) return;
    const request = cameraMergeRequestRef.current;
    const mergeProjectId = projectId;
    const preview = cameraMergePreview;
    setCameraMergeBusy(true);
    setCameraMergeError("");
    try {
      const report = await api.mergeCamera(
        mergeProjectId,
        preview.sourceCamera,
        preview.destinationCamera,
      );
      if (request !== cameraMergeRequestRef.current) return;
      cameraMergeRequestRef.current += 1;
      setCameraMergeSource("");
      setCameraMergeDestination("");
      setCameraMergePreview(null);
      const completion = `Merged ${report.sourceCamera} into ${report.destinationCamera}: ${report.movedClipCount} clips and ${report.movedCaptureSequenceCount} cards moved`;
      setMessage(completion);
      try {
        await Promise.all([reloadSessionAssets(), session.reloadChronology()]);
      } catch (reason) {
        setError(`Camera merge completed, but the refreshed view failed: ${String(reason)}`);
      }
    } catch (reason) {
      setCameraMergeError(String(reason));
    } finally {
      setCameraMergeBusy(false);
    }
  }

  async function makeMissingThumbnails() {
    if (!projectId || busy) return;
    setShowMediaMenu(false);
    setBusy(true);
    activeOperationRef.current = "thumbnail";
    setOperationProgress(initialProgress("thumbnail", missingThumbnailCount));
    setMessage("Generating thumbnails…");
    setError("");
    try {
      const report = await api.generateMissingThumbnails(projectId, 1);
      await reloadSessionAssets();
      setMessage(
        `Generated ${report.generated} thumbnails; ${report.skipped} already ready${
          report.failures.length ? `; ${report.failures.length} failed` : ""
        }`,
      );
      if (report.failures.length) setError(report.failures[0].error);
    } catch (reason) {
      setError(String(reason));
    } finally {
      activeOperationRef.current = null;
      setOperationProgress(null);
      setBusy(false);
    }
  }

  async function applyOffset() {
    if (!selected || !projectId) return;
    setBusy(true);
    setError("");
    try {
      await api.applyClockEvent(
        projectId,
        selected.camera,
        selected.filename,
        offsetMinutes * 60,
        offsetLabel,
      );
      await reloadSessionAssets();
      setMessage(`Reordered ${selected.camera} from ${selected.filename}`);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  const mediaOrigin = settings.mediaOrigin ?? null;
  // The loopback origin is part of every media URL, so a restarted origin has
  // to rebuild them — hence the origin in the dependency list as well as here.
  const bufferedMedia = useMemo(
    () => buildMediaBuffer(filteredAssets, selectedId, mediaOrigin, api.thumbnailUrl),
    [filteredAssets, selectedId, mediaOrigin],
  );
  const activeMedia = bufferedMedia.find((entry) => entry.active) ?? null;
  const mediaUrl = activeMedia?.url ?? null;

  /**
   * A media element failing is the only cheap signal that the loopback origin
   * serving review media has died. Check it, and take a new origin if the app
   * managed to restart one — every URL on screen is rebuilt from it. A healthy
   * origin means the failure was the file's, not the transport's, and nothing
   * here disturbs it.
   */
  const mediaRecoveryInFlight = useRef(false);
  const handleMediaError = useCallback(async () => {
    if (mediaRecoveryInFlight.current || !api.isDesktop()) return;
    mediaRecoveryInFlight.current = true;
    try {
      const origin = await api.recoverMediaOrigin();
      if (!origin) {
        setError("Review media is unavailable: the local media server did not start. Relaunch the app.");
        return;
      }
      if (origin.baseUrl === mediaOrigin?.baseUrl && origin.token === mediaOrigin.token) return;
      setSettings((current) => ({ ...current, mediaOrigin: origin }));
      setMessage("Reconnected to the local media server");
    } catch (reason) {
      setError(String(reason));
    } finally {
      mediaRecoveryInFlight.current = false;
    }
  }, [mediaOrigin]);
  const shouldAutoplay = Boolean(
    selected && mediaUrl && (
      settings.autoplayMode === "always"
      || (settings.autoplayMode === "proxyAvailable" && selected.proxyAvailable)
    ),
  );
  const labeledCount = activeAssets.filter((asset) => asset.labels.length > 0).length;
  const removedCount = assets.length - activeAssets.length;
  const missingProxyCount = assets.filter(
    (asset) => !asset.proxyAvailable || asset.proxyRepresentation !== "raw_v3",
  ).length;
  const missingThumbnailCount = assets.filter((asset) => asset.thumbnailQuality < 1 || !asset.thumbnailPath).length;

  useEffect(() => {
    setPlaybackSeconds(0);
    setMediaDuration(selected?.durationSeconds ?? 0);
    setIsPlaying(false);
    setIsBuffering(Boolean(mediaUrl));
  }, [mediaUrl, selected?.durationSeconds, selectedId]);

  useEffect(() => {
    viewerStageRef.current?.querySelectorAll<HTMLVideoElement>("video[data-asset-id]").forEach((candidate) => {
      candidate.muted = isMuted;
    });
  }, [isMuted, mediaUrl]);

  useEffect(() => {
    viewerStageRef.current?.querySelectorAll<HTMLVideoElement>("video[data-asset-id]").forEach((candidate) => {
      if (candidate.dataset.assetId !== selectedId) candidate.pause();
    });
    const video = videoRef.current;
    if (!video) return;
    setMediaDuration(Number.isFinite(video.duration) ? video.duration : selected?.durationSeconds ?? 0);
    setPlaybackSeconds(video.currentTime);
    setIsBuffering(video.readyState < HTMLMediaElement.HAVE_FUTURE_DATA);
    if (shouldAutoplay) void video.play().catch(() => {});
    else video.pause();
  }, [mediaUrl, selected?.durationSeconds, selectedId, shouldAutoplay]);

  function togglePlayback() {
    const video = videoRef.current;
    if (!video) return;
    if (video.paused) void video.play();
    else video.pause();
  }

  function seekBy(seconds: number) {
    const video = videoRef.current;
    if (!video) return;
    const duration = Number.isFinite(video.duration) ? video.duration : selected?.durationSeconds ?? 0;
    video.currentTime = Math.min(duration, Math.max(0, video.currentTime + seconds));
    setPlaybackSeconds(video.currentTime);
  }

  /**
   * The transport's ⤢: Focus with nothing over the frame (issue #7).
   *
   * There is one fullscreen concept, and it is Focus. Native fullscreen was a
   * second one with its own state, its own exit, and no relationship to the
   * `F` the same button advertised.
   */
  function toggleFrameOnly() {
    if (view.presentation === "focus" && !view.overlays) dispatchView({ kind: "exitFocus" });
    else dispatchView({ kind: "enterFocus", overlays: false });
  }

  function toggleMute() {
    setIsMuted((current) => {
      const next = !current;
      if (videoRef.current) videoRef.current.muted = next;
      return next;
    });
  }

  // ── Server pairing handlers ─────────────────────────────────────────

  async function handleTestServerConnection() {
    setBusy(true);
    setError("");
    try {
      const identity = await api.fetchServerInfo(serverEndpoint.trim());
      setServerIdentity(identity);
      setPairingWizardStep("code");
    } catch (err: any) {
      setError(`Cannot reach server: ${err}`);
    } finally {
      setBusy(false);
    }
  }

  async function handlePairWithServer() {
    setBusy(true);
    setError("");
    try {
      const result = await api.pairWithServer(serverEndpoint.trim(), pairingCode.trim(), deviceLabel.trim() || "MacBook Pro");
      const status = await api.serverConnectionStatus();
      setServerStatus(status);
      setShowPairingWizard(false);
      setPairingWizardStep("endpoint");
      setPairingCode("");
      setServerIdentity(null);
      setMessage(`Connected to ${result.label} (${result.nodeId.slice(0, 12)}…)`);
    } catch (err: any) {
      setError(`Pairing failed: ${err}`);
      setPairingWizardStep("code");
    } finally {
      setBusy(false);
    }
  }

  async function handleUnpairServer() {
    setBusy(true);
    setError("");
    try {
      await api.unpairServer();
      setServerStatus({ paired: false, nodeId: null, endpoint: null, label: null, pairedAt: null, endpoints: null });
      setMessage("Disconnected from server.");
    } catch (err: any) {
      setError(`Failed to unpair: ${err}`);
    } finally {
      setBusy(false);
    }
  }

  // ── Baseline pairing handlers ───────────────────────────────────────

  async function handleStartBaselineWizard(projectId: string) {
    setPairingProjectId(projectId);
    setBaselineStep("root_mapping");
    setBaselineError("");
    setShowBaselineWizard(true);

    // Load path analysis
    setBusy(true);
    try {
      const analysis = await api.analyzeProjectPaths(projectId);
      setPathAnalysis(analysis);
      const mappings = await api.proposeRootMappings(analysis);
      setRootMappings(mappings);
    } catch (err: any) {
      setBaselineError(`Failed to analyze project paths: ${err}`);
    } finally {
      setBusy(false);
    }
  }

  async function handleProceedToPreview() {
    setBaselineStep("preview");
    setBaselineError("");
    setBusy(true);
    try {
      const result = await api.previewProjectBaseline(pairingProjectId);
      setBaselinePreview(result);
    } catch (err: any) {
      setBaselineError(`Preview failed: ${err}`);
      setBaselineStep("root_mapping");
    } finally {
      setBusy(false);
    }
  }

  async function handleConfirmBaseline() {
    if (!baselinePreview) return;
    setBaselineStep("confirming");
    setBaselineError("");
    setBusy(true);
    try {
      await api.confirmBaselineImport(pairingProjectId, baselinePreview.digest);
      setBaselineStep("complete");
      setMessage("Project paired with server successfully.");
    } catch (err: any) {
      setBaselineError(`Import failed: ${err}`);
      setBaselineStep("error");
    } finally {
      setBusy(false);
    }
  }

  async function handleRetryBaseline() {
    setBaselineStep("preview");
    await handleProceedToPreview();
  }

  function handleCancelBaselineWizard() {
    api.abandonBaselineAttempt().catch(() => {});
    setShowBaselineWizard(false);
    setPathAnalysis(null);
    setRootMappings([]);
    setBaselinePreview(null);
    setBaselineError("");
  }

  // What the ribbon would be tall if nothing had been dragged — the value the
  // ribbon splitter starts from and the one double-click restores.
  const ribbonContentHeight = ribbonHeight(lanes.length, !view.ribbon);

  const seekMaximum = Math.max(0, mediaDuration || selected?.durationSeconds || 0);
  const seekProgress = seekMaximum > 0 ? Math.min(100, (playbackSeconds / seekMaximum) * 100) : 0;

  // Focus is a presentation, not a separate tree: these classes restyle what
  // is already mounted. That is what makes the caret and the playback position
  // survive the transition — the textarea and the video element are never
  // unmounted, so there is nothing to restore.
  const shellClass = [
    "app-shell",
    view.presentation === "focus" ? "is-focus" : "",
    view.presentation === "focus" && !view.overlays ? "overlays-hidden" : "",
    view.presentation === "focus" && view.rightStack ? "show-right-stack" : "",
  ].filter(Boolean).join(" ");

  return (
    <main className={shellClass}>
      {mountedCard && !showImportDialog && view.mode === "review" && (
        <CardBanner card={mountedCard} onReview={reviewMountedCard} onDismiss={dismissMountedCard} />
      )}
      <TopBar
        projects={projects}
        projectId={projectId}
        mode={view.mode}
        isDesktop={api.isDesktop()}
        busy={busy}
        canPairWithServer={Boolean(serverStatus?.paired && projectId)}
        syncStatus={syncStatus}
        operationProgress={operationProgress}
        error={error}
        message={message}
        missingProxyCount={missingProxyCount}
        missingThumbnailCount={missingThumbnailCount}
        missingAnalysisCount={readiness?.missingAnalyses ?? 0}
        mediaMenuOpen={showMediaMenu}
        hasLastImport={Boolean(importReport) || importSessions.length > 0}
        interruptedImportLabel={
          interruptedImport && interruptedImport.projectId === projectId
            ? interruptedMenuNote(interruptedImport, Boolean(resumableCard))
            : ""
        }
        labeledCount={labeledCount}
        activeCount={activeAssets.length}
        onProjectChange={setProjectId}
        onModeChange={(mode) => {
          if (mode === "chronology") openChronology();
          else if (mode === "review") returnToReview();
          else dispatchView({ kind: "setMode", mode });
        }}
        onOpenCatalog={openCatalog}
        onOpenSettings={() => setShowSettings(true)}
        onOpenCreateProject={() => setShowCreate(true)}
        onOpenShortcuts={() => setShowShortcuts(true)}
        onPairWithServer={() => void handleStartBaselineWizard(projectId)}
        onShowSyncDiagnostics={() => setShowSyncDiagnostics(true)}
        onCancelAnalysis={stopAnalysis}
        onCancelImport={() => void api.cancelImport()}
        onDismissError={() => setError("")}
        onMakeProxies={makeMissingProxies}
        onToggleMediaMenu={() => setShowMediaMenu((current) => !current)}
        onMakeThumbnails={makeMissingThumbnails}
        onOpenAnalysis={openAnalysisDialog}
        onImportFolder={importFolder}
        onReviewLastImport={() => importReport ? (setShowMediaMenu(false), setShowImportDialog(true)) : void openImportHistory()}
        onResumeInterruptedImport={reopenInterruptedImport}
      />

      {view.mode !== "review" && projectId ? (
        <div className={`mode-surface mode-${view.mode}`}>
          {view.mode === "chronology" ? (
            <CalibrationPanel
              projectId={projectId}
              currentAssetId={selectedId || null}
              assets={assets}
              overview={chronologyOverview}
              overviewError={session.chronologyError}
              reviewZoom={chronologyZoom}
              reviewScrollLeft={chronologyPreserveRibbonViewportRef.current
                ? reviewRibbonScrollLeftRef.current
                : undefined}
              focusAssetId={chronologyFocusAssetId}
              mediaOrigin={mediaOrigin}
              onZoomChange={setChronologyZoom}
              onBackToReview={returnToReview}
              // A correction can reorder hundreds of clips; resume where the
              // user was rather than snapping them back to the top.
              onQueueResumed={(assetId) => setSelectedId(assetId)}
              onChronologyChanged={() => Promise.all([reloadSessionAssets(), session.reloadChronology()]).then(() => undefined)}
            />
          ) : (
            <EditPlanPanel projectId={projectId} assets={assets} />
          )}
        </div>
      ) : (
      <div
        className="workbench"
        // Focus collapses the workbench to one column (styles.css); the panel
        // sizes only apply in Review. Applying them inline in Focus too would
        // silently win over that rule and strand the viewer between an empty
        // queue column and an empty rail column.
        style={
          view.presentation === "focus"
            ? undefined
            : { gridTemplateColumns: `${panelSizes.queue}px 5px minmax(0, 1fr) 5px ${panelSizes.rail}px` }
        }
      >
        <QueuePanel
          projectName={project?.name ?? null}
          assets={filteredAssets}
          selectedId={selectedId}
          query={query}
          analysisIndex={analysisIndex}
          showRemoved={showRemoved}
          showUnusableOnly={showUnusableOnly}
          unusableCount={unusableCount}
          removedCount={removedCount}
          listRef={queueRef}
          loading={session.loading}
          loadError={session.loadError}
          onReload={() => { void reloadSession(); }}
          onQueryChange={setQuery}
          onSearchSubmit={jumpToSpokenMatch}
          onSelect={setSelectedId}
          onToggleRemovedView={() => setShowRemoved((current) => !current)}
          onToggleUnusableView={() => setShowUnusableOnly((current) => !current)}
        />
        <Splitter
          orientation="vertical"
          value={panelSizes.queue}
          direction={1}
          label="Resize the clip queue"
          onResize={(next) => resizePanel("queue", next)}
          onCommit={commitPanelSizes}
          onReset={() => { resizePanel("queue", DEFAULT_PANEL_SIZES.queue); commitPanelSizes(); }}
        />

        <section className="viewer-panel">
          <VideoStage
            selected={selected}
            media={bufferedMedia}
            mediaUrl={mediaUrl}
            playing={isPlaying}
            buffering={isBuffering}
            muted={isMuted}
            autoplay={shouldAutoplay}
            stageRef={viewerStageRef}
            videoRef={videoRef}
            onTogglePlay={togglePlayback}
            onBufferingChange={setIsBuffering}
            onPlayingChange={setIsPlaying}
            onDurationChange={setMediaDuration}
            onPositionChange={setPlaybackSeconds}
            onMediaError={handleMediaError}
          />

          {selected && (
          <TransportBar
            asset={selected}
            playing={isPlaying}
            muted={isMuted}
            positionSeconds={playbackSeconds}
            durationSeconds={seekMaximum}
            bestMomentMs={observation?.analysis.bestMomentStartMs ?? null}
            segmentStartsMs={(transcript?.segments ?? []).map((segment) => segment.startMs)}
            rotationOverridden={selected.rotationDegrees !== selected.defaultRotationDegrees}
            onTogglePlay={togglePlayback}
            onSeek={seekTo}
            onSeekBy={seekBy}
            onToggleMute={toggleMute}
            onStep={moveSelection}
            onRotate={(addition) => void persistAsset(selected, { rotationDegrees: rotationAfter(selected.rotationDegrees, addition) })}
            onSetRotation={(degrees) => void persistAsset(selected, { rotationDegrees: degrees })}
            onResetRotation={() => void persistAsset(selected, { rotationDegrees: selected.defaultRotationDegrees })}
            frameOnly={view.presentation === "focus" && !view.overlays}
            onToggleFrameOnly={toggleFrameOnly}
          />
          )}

          {selected && (
            <EvidenceShelf
              asset={selected}
              transcript={transcript}
              observation={observation}
              open={view.shelf}
              tab={shelfTab}
              positionSeconds={playbackSeconds}
              query={query}
              onTabChange={setShelfTab}
              onToggleOpen={() => dispatchView({ kind: "toggleShelf" })}
              onSeek={seekTo}
              onChangeAsset={replaceAsset}
              onCommitAsset={() => void saveClipSummary(selected)}
              onLocateOriginals={() => void locateOriginals(selected)}
              busy={busy}
            />
          )}
        </section>

        <Splitter
          orientation="vertical"
          value={panelSizes.rail}
          direction={-1}
          label="Resize the notes panel"
          onResize={(next) => resizePanel("rail", next)}
          onCommit={commitPanelSizes}
          onReset={() => { resizePanel("rail", DEFAULT_PANEL_SIZES.rail); commitPanelSizes(); }}
        />
        {selected ? (
          <JudgmentPanel
            asset={selected}
            index={filteredAssets.findIndex((candidate) => candidate.id === selected.id) + 1}
            observation={observation}
            labelDraft={labelDraft}
            labelVocabulary={labelVocabulary}
            saveState={mutations.saveStateFor(selected.id)}
            saveError={mutations.drafts[selected.id]?.error ?? ""}
            onRetrySave={() => { void mutations.retryDraft(selected); }}
            onLabelDraftChange={setLabelDraft}
            onAddLabel={(value) => addLabel(selected, value)}
            onRemoveLabel={(label) => removeLabel(selected, label)}
            onPromoteAiLabel={(label) => addLabel(selected, label)}
            onChangeAsset={replaceAsset}
            onCommitAsset={() => void saveClipSummary(selected)}
            onToggleRemoved={() => toggleRemoved(selected)}
            onNotesFocus={() => {
              notesEditingRef.current = { assetId: selected.id, savedNotes: selected.notes, cancelOnBlur: false };
            }}
            onNotesKeyDown={handleNotesKeyDown}
            onNotesBlur={handleNotesBlur}
          />
        ) : (
          <aside className="judgment-panel">
            <div className="empty-inspector">Select a clip to log it.</div>
          </aside>
        )}
      </div>
      )}

      {/* Third and last shell child. Full width, so the ribbon sees 1440px
          instead of the viewer column's 863px, and the viewer column stops
          paying for it. */}
      {view.mode === "review" && (
      <footer className="story-ribbon">
        {/* Inside the footer rather than beside it: the shell holds exactly
            three children, and a fourth would take an implicit grid row —
            the bug this layout started out fixing. */}
        {view.ribbon && view.presentation !== "focus" && (
          <Splitter
            orientation="horizontal"
            value={panelSizes.ribbon ?? ribbonContentHeight}
            direction={-1}
            label="Resize the story ribbon"
            onResize={(next) => resizePanel("ribbon", next)}
            onCommit={commitPanelSizes}
            onReset={() => { setPanelSizes((current) => ({ ...current, ribbon: null })); commitPanelSizes(); }}
          />
        )}
        <StoryRibbon
          assets={activeAssets}
          lanes={lanes}
          contextRuns={contextRuns}
          selected={selected}
          selectedId={selectedId}
          selectedTimeLabel={selected
            ? formatChronologyTime(
                chronologyAssetsById.get(selected.id)?.displayTime?.primary ?? selected.bestTime,
                chronologyAssetsById.get(selected.id)?.displayTime?.timezone,
              )
            : null}
          zoom={chronologyZoom}
          scrollRef={chronologyScrollRef}
          collapsed={!view.ribbon}
          merged={view.presentation === "focus"}
          height={view.presentation === "focus" ? null : panelSizes.ribbon}
          onZoomChange={setChronologyZoom}
          onCenter={() => centerSelectedInChronology()}
          onSelect={setSelectedId}
          onToggleCollapsed={() => dispatchView({ kind: "toggleRibbon" })}
          onRequestContext={openContextRange}
          onEditContext={openExistingContext}
          chronologyProblemCount={chronologyProblemCount}
          chronologyQuestionCount={chronologyQuestionCount}
          onOpenChronologyProblems={() => openChronology(firstChronologyProblemAssetId, false)}
          onOpenExpanded={() => openChronology(selectedId, true)}
          onRenameCamera={(from, to) => { void renameCamera(from, to); }}
          onRequestCameraMerge={openCameraMerge}
        />
      </footer>
      )}

      {phaseDialog && activeAssets[phaseDialog.startIndex] && activeAssets[phaseDialog.endIndex] && (
        <TripPhaseDialog
          key={`${phaseDialog.phase?.id ?? "new"}:${phaseDialog.startIndex}:${phaseDialog.endIndex}`}
          phase={phaseDialog.phase}
          start={activeAssets[phaseDialog.startIndex]}
          end={activeAssets[phaseDialog.endIndex]}
          assets={activeAssets}
          startPosition={phaseDialog.startIndex}
          endPosition={phaseDialog.endIndex}
          busy={phaseDialogBusy}
          error={phaseDialogError}
          onCancel={() => setPhaseDialog(null)}
          onSave={(value, range) => { void saveTripPhase(value, range); }}
          onRemove={phaseDialog.phase ? () => { void removeTripPhase(); } : undefined}
        />
      )}

      {showCreate && (
        <NewProjectModal
          library={library}
          isDesktop={api.isDesktop()}
          busy={busy}
          newProjectName={newProjectName}
          canCancel={projects.length > 0}
          onChangeRoot={configureProjectsRoot}
          onOpenProject={openLibraryProject}
          onNewProjectNameChange={setNewProjectName}
          onOpenCatalog={openCatalog}
          onCancel={() => setShowCreate(false)}
          onCreate={() => void createProject()}
        />
      )}

      {showSettings && (
        <SettingsModal
          settings={settings}
          libraryRootPath={library.rootPath}
          isDesktop={api.isDesktop()}
          busy={busy}
          models={models}
          modelInstall={modelInstall}
          keyInput={keyInput}
          keyPresent={keyPresent}
          serverStatus={serverStatus}
          onAutoplayModeChange={(mode) => void updateAutoplayMode(mode)}
          onChangeProjectsRoot={() => void configureProjectsRoot()}
          onInstallModels={() => void installModels()}
          onKeyInputChange={setKeyInput}
          onSaveKey={() => void saveOpenRouterKey()}
          onRemoveKey={() => { setKeyInput(""); void api.setOpenRouterKey("").then(setKeyPresent); }}
          onConnectServer={() => setShowPairingWizard(true)}
          onDisconnectServer={() => void handleUnpairServer()}
          onClose={() => setShowSettings(false)}
        />
      )}

      {/* ── Pairing wizard ─────────────────────────────────────────── */}
      {showPairingWizard && (
        <PairingWizardModal
          step={pairingWizardStep}
          serverEndpoint={serverEndpoint}
          serverIdentity={serverIdentity}
          pairingCode={pairingCode}
          deviceLabel={deviceLabel}
          busy={busy}
          onServerEndpointChange={setServerEndpoint}
          onTestConnection={() => void handleTestServerConnection()}
          onPairingCodeChange={setPairingCode}
          onDeviceLabelChange={setDeviceLabel}
          onPair={() => void handlePairWithServer()}
          onBack={() => setPairingWizardStep("endpoint")}
          onCancel={closePairingWizard}
        />
      )}

      {/* ── Baseline pairing wizard ────────────────────────────────── */}
      {showBaselineWizard && (
        <BaselineWizardModal
          step={baselineStep}
          error={baselineError}
          pathAnalysis={pathAnalysis}
          rootMappings={rootMappings}
          preview={baselinePreview}
          busy={busy}
          onPreview={() => void handleProceedToPreview()}
          onBack={() => setBaselineStep("root_mapping")}
          onConfirm={() => void handleConfirmBaseline()}
          onRetry={() => void handleRetryBaseline()}
          onDone={() => { setShowBaselineWizard(false); setBaselineStep("root_mapping"); setPathAnalysis(null); setRootMappings([]); setBaselinePreview(null); }}
          onCancel={handleCancelBaselineWizard}
        />
      )}

      {showAnalysisDialog && readiness && (
        <AnalysisDialog
          projectName={project?.name ?? "This project"}
          readiness={readiness}
          budget={budgetInput}
          forceReanalyze={forceReanalyze}
          onBudgetChange={setBudgetInput}
          onForceReanalyzeChange={setForceReanalyze}
          onOpenSettings={() => { setShowAnalysisDialog(false); setShowSettings(true); }}
          onCancel={() => setShowAnalysisDialog(false)}
          onStart={runAnalysis}
        />
      )}

      {cameraMergeSource && (
        <CameraMergeDialog
          sourceCamera={cameraMergeSource}
          destinationCameras={projectCameraIdentities.filter((camera) => camera !== cameraMergeSource)}
          destinationCamera={cameraMergeDestination}
          preview={cameraMergePreview}
          busy={cameraMergeBusy}
          error={cameraMergeError}
          onDestinationChange={(camera) => void selectCameraMergeDestination(camera)}
          onConfirm={() => void confirmCameraMerge()}
          onCancel={closeCameraMerge}
        />
      )}

      {showShortcuts && <KeyboardShortcutsModal onClose={() => setShowShortcuts(false)} />}
      {/* ── Sync diagnostics ─────────────────────────────────────── */}
      {showSyncDiagnostics && syncStatus && (
        <SyncDiagnosticsModal
          status={syncStatus}
          onSyncNow={() => { api.syncNow(syncStatus.projectId).catch(() => {}); }}
          onClose={() => setShowSyncDiagnostics(false)}
        />
      )}

      {showImportDialog && (
        <ImportDialog
          projectName={importProjectName || "this project"}
          sourcePath={importSource}
          card={importCard}
          roots={archiveRoots}
          selectedRootId={selectedArchiveRootId}
          plan={importPlan}
          loading={importLoading}
          running={importRunning}
          progress={importProgress}
          report={importReport}
          error={importError}
          interrupted={importDisconnected ? interruptedImport : null}
          reconnected={Boolean(resumableCard)}
          verifyCopies={verifyImportCopies}
          makeProxies={makeImportProxies}
          knownCameras={importKnownCameras}
          mappings={importCameraMappings}
          onSelectRoot={(id) => void refreshImportPlan(id)}
          onCreateRoot={(name) => void createImportArchive(name)}
          onLocateRoot={(root) => void locateImportArchive(root)}
          onVerifyCopies={setVerifyImportCopies}
          onMakeProxies={setMakeImportProxies}
          onSelectCamera={(camera) => void selectImportCamera(camera)}
          onRenameMapping={(id, camera) => void renameImportCameraMapping(id, camera)}
          onClearMapping={(id) => void clearImportCameraMapping(id)}
          onImport={() => void runSmartImport()}
          onCancel={() => void api.cancelImport()}
          onClose={closeImportDialog}
          onResume={() => void resumeInterruptedImport(resumableCard)}
          onChooseSource={() => void chooseInterruptedImportSource()}
          onCancelInterrupted={cancelInterruptedImport}
          onSetCutoff={(item) => void setImportCutoff(item)}
          onClearCutoff={() => void clearImportCutoff()}
        />
      )}
      {showImportHistory && (
        <ImportHistoryDialog
          sessions={importSessions}
          detail={importSessionDetail}
          preview={importRollback}
          error={importHistoryError}
          busy={busy}
          onOpen={(id) => void openImportSession(id)}
          onPreview={(id) => void previewImportRollback(id)}
          onUndo={(id) => void rollbackImportSession(id)}
          onClose={() => (setShowImportHistory(false), setImportHistoryError(""))}
        />
      )}

    </main>
  );
}

export default App;
