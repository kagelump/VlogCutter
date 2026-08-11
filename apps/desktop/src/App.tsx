import { useCallback, useEffect, useMemo, useReducer, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { api } from "./api";
import type { ArchiveRoot, Asset, AssetPatch, AutoplayMode, BaselineImportResult, BaselinePreviewResult, CardCandidate, DesktopSettings, ImportHistoryEntry, ImportPreflight, ImportProgress, ImportRollbackPreview, ImportRunReport, ImportSessionDetail, ModelInstallProgress, ModelStatus, PathAnalysis, Project, ProjectLibrary, ProjectLocation, ProjectPairingStatus, RootMappingEntry, ServerConnectionStatus, ServerVersionInfo, SyncStatus, TranscriptStatus } from "./types";
import { useProjectSession } from "./session/projectSession";
import { useAssetMutations } from "./session/assetMutations";
import { orderAssets, moveSelection as moveSelectionInOrder } from "./review/order";
import { resolveAction } from "./review/keymap";
import { CalibrationPanel } from "./CalibrationPanel";
import { chronologyApi } from "./chronologyApi";
import type { ChronologyOverview, TripPhase } from "./chronologyTypes";
import { EditPlanPanel } from "./EditPlanPanel";
import { formatBytes, transcriptStatusLabels } from "./review/summaries";
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
import { CardBanner } from "./components/CardBanner";
import { analysisStageLabels, formatCost } from "./review/format";
import { isRawOrientationProxy, renderedRotation, rotationAfter } from "./review/rotation";

const autoplayOptions: Array<{ value: AutoplayMode; label: string; detail: string }> = [
  { value: "never", label: "Never", detail: "Keep each clip paused until you press Space." },
  { value: "proxyAvailable", label: "Only when proxy is available", detail: "Autoplay lightweight review media; keep camera originals paused." },
  { value: "always", label: "Always", detail: "Autoplay whichever version is available, including camera originals." },
];

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
  const [isViewerFullscreen, setIsViewerFullscreen] = useState(false);
  const [offsetMinutes, setOffsetMinutes] = useState(0);
  const [offsetLabel, setOffsetLabel] = useState("Camera time change");
  const [chronologyZoom, setChronologyZoom] = useState(RIBBON_ZOOM_DEFAULT);
  const [phaseDialog, setPhaseDialog] = useState<PhaseDialogState | null>(null);
  const [phaseDialogBusy, setPhaseDialogBusy] = useState(false);
  const [phaseDialogError, setPhaseDialogError] = useState("");
  const [showImportDialog, setShowImportDialog] = useState(false);
  const [importProjectName, setImportProjectName] = useState("");
  const [importSource, setImportSource] = useState("");
  const [importCard, setImportCard] = useState<CardCandidate | null>(null);
  const [archiveRoots, setArchiveRoots] = useState<ArchiveRoot[]>([]);
  const [selectedArchiveRootId, setSelectedArchiveRootId] = useState("");
  const [importPlan, setImportPlan] = useState<ImportPreflight | null>(null);
  const [importProgress, setImportProgress] = useState<ImportProgress | null>(null);
  const [importReport, setImportReport] = useState<ImportRunReport | null>(null);
  const [importLoading, setImportLoading] = useState(false);
  const [importRunning, setImportRunning] = useState(false);
  const [showImportHistory, setShowImportHistory] = useState(false);
  const [importSessions, setImportSessions] = useState<ImportHistoryEntry[]>([]);
  const [importSessionDetail, setImportSessionDetail] = useState<ImportSessionDetail | null>(null);
  const [importRollback, setImportRollback] = useState<ImportRollbackPreview | null>(null);
  const [importError, setImportError] = useState("");
  const [verifyImportCopies, setVerifyImportCopies] = useState(true);
  const [makeImportProxies, setMakeImportProxies] = useState(true);
  const [mountedCard, setMountedCard] = useState<CardCandidate | null>(null);
  const dismissedCardSessionsRef = useRef(new Set<string>());
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
  const queueRef = useRef<HTMLDivElement>(null);
  const viewerStageRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const assetsRef = useRef<Asset[]>([]);
  const selectedIdRef = useRef("");
  // The clip to land on when a queue arrives with no live selection yet — the
  // review position recorded on last exit. Held in a ref so the assets read can
  // consult it without a dependency, and left in place: once the selection is
  // live, `onAssetsRead` keeps it and never consults this again.
  const restoreAssetIdRef = useRef("");
  const orientationQueueRef = useRef<string[]>([]);
  const orientationActiveRef = useRef(new Set<string>());

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
  const chronologyOverview = session.chronology;
  const reloadSession = session.reload;
  const reloadSessionAssets = session.reloadAssets;

  // One owner for the write path: optimistic apply, the stated failure policy,
  // save state, and the double-submit guard.
  const mutations = useAssetMutations({
    update: useCallback((assetId: string, patch: AssetPatch) => api.updateAsset(assetId, patch), []),
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

  const selected = assets.find((asset) => asset.id === selectedId) ?? null;
  const project = projects.find((item) => item.id === projectId) ?? null;

  useEffect(() => {
    assetsRef.current = assets;
    selectedIdRef.current = selectedId;
  }, [assets, selectedId]);

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

  const chronologyAssetsById = useMemo(
    () => new Map((chronologyOverview?.assets ?? []).map((asset) => [asset.assetId, asset])),
    [chronologyOverview],
  );
  const phasesById = useMemo(
    () => new Map((chronologyOverview?.phases ?? []).map((phase) => [phase.id, phase])),
    [chronologyOverview],
  );
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
    api.listImportSessions(projectId).then(setImportSessions).catch(() => setImportSessions([]));
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
          patchAssets((current) => current.map((candidate) => candidate.id === updated.id ? updated : candidate));
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
            patchAssets((current) => current.map((asset) => asset.id === progress.asset?.id ? progress.asset : asset));
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
            patchAssets((current) => current.map((asset) => asset.id === progress.asset?.id ? progress.asset : asset));
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
            patchAssets((current) => current.map((asset) => asset.id === progress.asset?.id ? progress.asset : asset));
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
          setImportProgress(progress);
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
    };
  }, [patchAssets]);

  // Mount detection is deliberately low-frequency and listing-only. It offers
  // a review action; hashing and probing begin only after that click.
  useEffect(() => {
    if (!api.isDesktop() || !projectId) {
      setMountedCard(null);
      return;
    }
    let disposed = false;
    const poll = async () => {
      try {
        const cards = await api.detectMountedCards();
        if (disposed || showImportDialog) return;
        setMountedCard(cards.find((card) => !dismissedCardSessionsRef.current.has(card.sessionId)) ?? null);
      } catch {
        if (!disposed) setMountedCard(null);
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 15_000);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [projectId, showImportDialog]);

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
    const label = value.trim();
    if (!label) return;
    setLabelDraft("");
    if (asset.labels.some((current) => current.toLocaleLowerCase() === label.toLocaleLowerCase())) return;
    void persistAsset(asset, { labels: [...asset.labels, label] });
  }, [persistAsset]);

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

  async function saveTripPhase(value: TripPhaseFormValue) {
    if (!phaseDialog || !projectId) return;
    const start = activeAssets[phaseDialog.startIndex];
    const end = activeAssets[phaseDialog.endIndex];
    if (!start || !end) return;
    const existing = phaseDialog.phase;
    setPhaseDialogBusy(true);
    setPhaseDialogError("");
    try {
      const draft: TripPhase = {
        id: existing?.id ?? "",
        label: value.label,
        kind: value.kind,
        startAnchorAssetId: existing
          ? existing.startAnchorAssetId
          : start.id,
        endAnchorAssetId: existing
          ? existing.endAnchorAssetId
          : end.id,
        startTripTime: existing?.startTripTime ?? start.bestTime ?? start.capturedAtRaw ?? "",
        endTripTime: existing?.endTripTime ?? end.bestTime ?? end.capturedAtRaw ?? "",
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
        // Modals first, then Focus: Escape should close what is on top.
        const hadModal = showMediaMenu || showShortcuts || showSettings || showAnalysisDialog;
        setShowMediaMenu(false);
        setShowShortcuts(false);
        setShowSettings(false);
        setShowAnalysisDialog(false);
        if (projects.length > 0) setShowCreate(false);
        if (!hadModal && view.presentation === "focus") dispatchView({ kind: "exitFocus" });
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
              dispatchView({ kind: "setMode", mode: reviewAction.mode });
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
            // Shift+T's expanded overlay is not built; swallow the key rather
            // than letting it reach an unrelated handler.
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
  }, [busy, centerSelectedInChronology, moveSelection, observation, persistAsset, projectId, projects.length, selected, showAnalysisDialog, showMediaMenu, showSettings, showShortcuts, toggleRemoved, view.presentation]);

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

  async function openImportForPath(path: string, knownCard?: CardCandidate) {
    setShowImportDialog(true);
    setImportProjectName(project?.name ?? "this project");
    setImportSource(path);
    setImportPlan(null);
    setImportProgress(null);
    setImportReport(null);
    setImportError("");
    setImportLoading(true);
    try {
      const [card, roots] = await Promise.all([
        knownCard ? Promise.resolve(knownCard) : api.inspectImportSource(path),
        api.listArchiveRoots(),
      ]);
      setImportCard(card);
      setArchiveRoots(roots);
      const mappedRootId = card
        ? settings.importArchiveRootIdsByCardSession[card.sessionId]
        : null;
      const remembered = roots.find((root) => root.id === mappedRootId)
        ?? roots.find((root) => root.id === settings.lastImportArchiveRootId);
      const selectedRoot = card ? (remembered ?? roots.find((root) => root.available) ?? roots[0] ?? null) : null;
      const rootId = selectedRoot?.id ?? "";
      setSelectedArchiveRootId(rootId);
      if (!card || rootId) {
        setImportPlan(await api.preflightImport(projectId, path, rootId || null));
      }
    } catch (reason) {
      if (!String(reason).includes("preflight cancelled")) setImportError(String(reason));
    } finally {
      setImportLoading(false);
    }
  }

  async function refreshImportPlan(rootId: string) {
    if (!projectId || !importSource || (importCard && !rootId)) {
      setImportPlan(null);
      return;
    }
    setSelectedArchiveRootId(rootId);
    setImportLoading(true);
    setImportError("");
    setImportProgress(null);
    try {
      setImportPlan(await api.preflightImport(projectId, importSource, rootId || null));
    } catch (reason) {
      if (!String(reason).includes("preflight cancelled")) setImportError(String(reason));
    } finally {
      setImportLoading(false);
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
    setImportRunning(true);
    setBusy(true);
    setImportProgress(null);
    setImportError("");
    activeOperationRef.current = "import";
    setOperationProgress(initialProgress("import", importPlan.newClips + importPlan.relinkable));
    setShowImportDialog(false);
    try {
      const nextSettings = await api.setImportPreferences(
        makeImportProxies,
        selectedArchiveRootId || null,
        importCard?.sessionId ?? null,
      );
      setSettings(nextSettings);
      const report = await api.runSmartImport(projectId, importPlan, verifyImportCopies, makeImportProxies);
      setImportReport(report);
      setImportSessions((current) => report.session ? [report.session, ...current.filter((entry) => entry.id !== report.session!.id)] : current);
      await Promise.all([reloadSessionAssets(), refreshProjects(), session.reloadReadiness()]);
      const failures = report.outcome.failures.length + (report.proxies?.failures.length ?? 0);
      if (failures) {
        const failureMessage = `${failures} file${failures === 1 ? "" : "s"} need attention in the import ledger.`;
        setImportError(failureMessage);
        setError(failureMessage);
      } else if (report.outcome.cancelled) {
        setMessage("Import stopped cleanly; finished files were kept");
      } else {
        setMessage(`Import complete · ${report.outcome.imported} clip${report.outcome.imported === 1 ? "" : "s"} added`);
      }
    } catch (reason) {
      setError(String(reason));
    } finally {
      activeOperationRef.current = null;
      setOperationProgress(null);
      setImportRunning(false);
      setBusy(false);
    }
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
    setShowImportDialog(false);
  }

  async function openImportHistory() {
    if (!projectId) return;
    setShowMediaMenu(false);
    setImportSessionDetail(null);
    setImportRollback(null);
    setImportSessions(await api.listImportSessions(projectId));
    setShowImportHistory(true);
  }

  async function openImportSession(sessionId: string) {
    if (!projectId) return;
    setImportRollback(null);
    setImportSessionDetail(await api.importSession(projectId, sessionId));
  }

  async function previewImportRollback(sessionId: string) {
    if (!projectId) return;
    setImportRollback(await api.importRollbackPreview(projectId, sessionId));
  }

  async function rollbackImportSession(sessionId: string) {
    if (!projectId) return;
    await api.rollbackImportMembership(projectId, sessionId);
    await Promise.all([reloadSessionAssets(), api.listImportSessions(projectId).then(setImportSessions)]);
    setImportRollback(null);
    setImportSessionDetail(await api.importSession(projectId, sessionId));
    setMessage("Import membership removed; original media was left untouched");
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
      await reloadSessionAssets();
      setMessage(`Renamed ${from} to ${to} on ${moved} ${moved === 1 ? "clip" : "clips"}`);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
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
  const bufferedMedia = useMemo(() => {
    const selectedIndex = filteredAssets.findIndex((asset) => asset.id === selectedId);
    if (selectedIndex < 0) return [];
    return [-1, 0, 1]
      .map((offset) => ({ asset: filteredAssets[selectedIndex + offset], offset }))
      .filter((entry): entry is { asset: Asset; offset: number } => Boolean(entry.asset?.orientationProbed && entry.asset.available))
      .map(({ asset, offset }) => {
        const url = api.mediaUrl(asset);
        const usingProxy = Boolean(asset.proxyAvailable && asset.proxyPath);
        const mediaIsRawOriented = usingProxy && isRawOrientationProxy(asset);
        const rotation = renderedRotation(asset, mediaIsRawOriented);
        const displayIsQuarterTurn = asset.rotationDegrees % 180 !== 0;
        const displayAspect = (displayIsQuarterTurn ? asset.height / asset.width : asset.width / asset.height) || 16 / 9;
        return {
          asset,
          offset,
          active: asset.id === selectedId,
          url,
          mediaIsRawOriented,
          rotation,
          displayAspect,
          posterUrl: api.thumbnailUrl(asset),
        };
      })
      .filter((entry) => Boolean(entry.url));
    // The loopback origin is part of every media URL, so a restarted origin has
    // to rebuild them; `api.mediaUrl` reads it from the last settings response.
  }, [filteredAssets, selectedId, mediaOrigin?.baseUrl, mediaOrigin?.token]);
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

  useEffect(() => {
    const handleFullscreenChange = () => {
      setIsViewerFullscreen(document.fullscreenElement === viewerStageRef.current);
    };
    document.addEventListener("fullscreenchange", handleFullscreenChange);
    return () => document.removeEventListener("fullscreenchange", handleFullscreenChange);
  }, []);

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

  async function toggleViewerFullscreen() {
    try {
      if (document.fullscreenElement) await document.exitFullscreen();
      else await viewerStageRef.current?.requestFullscreen();
    } catch (reason) {
      setError(`Fullscreen unavailable: ${String(reason)}`);
    }
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
      {mountedCard && !showImportDialog && (
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
        labeledCount={labeledCount}
        activeCount={activeAssets.length}
        onProjectChange={setProjectId}
        onModeChange={(mode) => dispatchView({ kind: "setMode", mode })}
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
      />

      {view.mode !== "review" && projectId ? (
        <div className="mode-surface">
          {view.mode === "chronology" ? (
            <CalibrationPanel
              projectId={projectId}
              currentAssetId={selectedId || null}
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
            onResetRotation={() => void persistAsset(selected, { rotationDegrees: selected.defaultRotationDegrees })}
            onEnterFocus={() => void toggleViewerFullscreen()}
          />
          )}

          {selected && (
            <EvidenceShelf
              asset={selected}
              transcript={transcript}
              observation={observation}
              open={view.shelf}
              tab={shelfTab}
              onTabChange={setShelfTab}
              onToggleOpen={() => dispatchView({ kind: "toggleShelf" })}
              onSeek={seekTo}
              onChangeAsset={replaceAsset}
              onCommitAsset={() => void saveClipSummary(selected)}
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
          onRenameCamera={(from, to) => { void renameCamera(from, to); }}
        />
      </footer>
      )}

      {phaseDialog && activeAssets[phaseDialog.startIndex] && activeAssets[phaseDialog.endIndex] && (
        <TripPhaseDialog
          key={`${phaseDialog.phase?.id ?? "new"}:${phaseDialog.startIndex}:${phaseDialog.endIndex}`}
          phase={phaseDialog.phase}
          start={activeAssets[phaseDialog.startIndex]}
          end={activeAssets[phaseDialog.endIndex]}
          startPosition={phaseDialog.startIndex}
          endPosition={phaseDialog.endIndex}
          busy={phaseDialogBusy}
          error={phaseDialogError}
          onCancel={() => setPhaseDialog(null)}
          onSave={(value) => { void saveTripPhase(value); }}
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
        <div className="modal-backdrop settings-backdrop" role="presentation">
          <section className="modal settings-modal" role="dialog" aria-modal="true" aria-labelledby="settings-title">
            <span className="eyebrow">Global settings</span>
            <div className="settings-modal-heading">
              <div>
                <h2 id="settings-title">Review behavior</h2>
                <p>These choices apply to every project on this Mac.</p>
              </div>
              <button className="text-button" onClick={() => setShowSettings(false)}>Close <kbd>Esc</kbd></button>
            </div>

            <fieldset className="settings-group">
              <legend>Autoplay clips</legend>
              <div className="settings-options">
                {autoplayOptions.map((option) => (
                  <label className={`settings-option ${settings.autoplayMode === option.value ? "selected" : ""}`} key={option.value}>
                    <input
                      type="radio"
                      name="autoplay-mode"
                      value={option.value}
                      checked={settings.autoplayMode === option.value}
                      autoFocus={settings.autoplayMode === option.value}
                      onChange={() => void updateAutoplayMode(option.value)}
                    />
                    <span>
                      <strong>{option.label}</strong>
                      <small>{option.detail}</small>
                    </span>
                  </label>
                ))}
              </div>
            </fieldset>

            <div className="settings-group project-folder-setting">
              <div>
                <span className="settings-legend">Default projects folder</span>
                <span className="project-root mono" title={settings.projectsRoot || library.rootPath}>{settings.projectsRoot || library.rootPath || "Loading…"}</span>
              </div>
              {api.isDesktop() && <button className="button secondary" disabled={busy} onClick={() => void configureProjectsRoot()}>Change…</button>}
            </div>

            <fieldset className="settings-group analysis-setting">
              <legend>Clip analysis</legend>

              <div className="settings-row">
                <div>
                  <span className="settings-legend">Speech models</span>
                  <small>
                    Downloaded once and shared by every project. Nothing else in the app downloads
                    them: a batch fails with an explanation instead.
                  </small>
                  <ul className="model-list">
                    {models.map((model) => (
                      <li key={model.name}>
                        <span className="mono">{model.name}</span>
                        <span>{model.installed ? "Installed" : `${Math.round(model.bytes / 1_000_000)} MB`}</span>
                      </li>
                    ))}
                  </ul>
                  {modelInstall && (
                    <div className="model-progress">
                      <div className="progress-track" role="progressbar" aria-valuemin={0} aria-valuemax={modelInstall.totalBytes} aria-valuenow={modelInstall.downloadedBytes}>
                        <span style={{ width: `${modelInstall.totalBytes ? (modelInstall.downloadedBytes / modelInstall.totalBytes) * 100 : 0}%` }} />
                      </div>
                      <span className="section-note">
                        {modelInstall.name || "Starting"} · {formatBytes(modelInstall.downloadedBytes)} of {formatBytes(modelInstall.totalBytes)}
                      </span>
                    </div>
                  )}
                </div>
                <button
                  className="button secondary"
                  disabled={Boolean(modelInstall) || models.every((model) => model.installed)}
                  onClick={() => void installModels()}
                >
                  {models.every((model) => model.installed) ? "Installed" : modelInstall ? "Downloading…" : "Install"}
                </button>
              </div>

              <div className="settings-row">
                <div>
                  <span className="settings-legend">OpenRouter key</span>
                  <small>
                    Stored in your macOS Keychain, never in a project catalog. Required only for the
                    cloud analysis step; transcription runs offline without it.
                  </small>
                  <label className="field-label">
                    <span className="sr-only">OpenRouter key</span>
                    <input
                      type="password"
                      value={keyInput}
                      placeholder={keyPresent ? "A key is stored. Enter a new one to replace it." : "sk-or-…"}
                      autoComplete="off"
                      onChange={(event) => setKeyInput(event.target.value)}
                    />
                  </label>
                </div>
                <div className="settings-row-actions">
                  <button className="button secondary" disabled={!keyInput.trim()} onClick={() => void saveOpenRouterKey()}>Save</button>
                  {keyPresent && (
                    <button className="text-button" onClick={() => { setKeyInput(""); void api.setOpenRouterKey("").then(setKeyPresent); }}>Remove</button>
                  )}
                </div>
              </div>
            </fieldset>

            <div className="settings-group server-setting">
              <div>
                <span className="settings-legend">Server connection</span>
                {serverStatus?.paired ? (
                  <div className="server-status connected">
                    <span className="server-dot" aria-label="connected"></span>
                    <span>{serverStatus.label || "Server"} — {serverStatus.nodeId?.slice(0, 12)}…</span>
                    {serverStatus.endpoints && serverStatus.endpoints.length > 0 && (
                      <div className="endpoint-list">
                        {serverStatus.endpoints.map((ep) => (
                          <span className="mono endpoint-tag" key={ep.url}>{ep.kind}: {ep.url}</span>
                        ))}
                      </div>
                    )}
                  </div>
                ) : (
                  <div className="server-status disconnected">
                    <span className="server-dot" aria-label="disconnected"></span>
                    <span>Not connected to a server</span>
                  </div>
                )}
              </div>
              {api.isDesktop() && (
                serverStatus?.paired ? (
                  <button className="button secondary" disabled={busy} onClick={() => void handleUnpairServer()}>Disconnect</button>
                ) : (
                  <button className="button primary" disabled={busy} onClick={() => setShowPairingWizard(true)}>Connect to server…</button>
                )
              )}
            </div>
          </section>
        </div>
      )}

      {/* ── Pairing wizard ─────────────────────────────────────────── */}
      {showPairingWizard && (
        <div className="modal-backdrop pairing-backdrop" role="presentation" onKeyDown={(event) => { if (event.key === "Escape") { setShowPairingWizard(false); setPairingWizardStep("endpoint"); setPairingCode(""); setServerIdentity(null); }}}>
          <section className="modal pairing-modal" role="dialog" aria-modal="true" aria-labelledby="pairing-title">
            <span className="eyebrow">Connect to server</span>
            <div className="settings-modal-heading">
              <h2 id="pairing-title">
                {pairingWizardStep === "endpoint" && "Enter server address"}
                {pairingWizardStep === "code" && "Enter pairing code"}
                {pairingWizardStep === "connecting" && "Connecting…"}
              </h2>
              <button className="text-button" onClick={() => { setShowPairingWizard(false); setPairingWizardStep("endpoint"); setPairingCode(""); setServerIdentity(null); }}>Cancel <kbd>Esc</kbd></button>
            </div>

            {pairingWizardStep === "endpoint" && (
              <div className="wizard-step">
                <p>Enter the HTTPS URL of your RCD VlogCutter server on the home network or Tailscale.</p>
                <label className="field">
                  <span>Server URL</span>
                  <input
                    type="text"
                    className="mono"
                    placeholder="https://rcdvlog.local"
                    value={serverEndpoint}
                    onChange={(e) => setServerEndpoint(e.target.value)}
                    onKeyDown={(e) => { if (e.key === "Enter") void handleTestServerConnection(); }}
                    autoFocus
                  />
                </label>
                <div className="modal-actions">
                  <button className="button primary" disabled={busy || !serverEndpoint.trim()} onClick={() => void handleTestServerConnection()}>Test connection</button>
                </div>
              </div>
            )}

            {pairingWizardStep === "code" && serverIdentity && (
              <div className="wizard-step">
                <div className="server-identity">
                  <span className="server-dot connected"></span>
                  <span>Server: <strong>{serverIdentity.nodeId.slice(0, 16)}…</strong></span>
                  <span className="mono quiet">v{serverIdentity.applicationVersion} · protocol {serverIdentity.protocolMajor}.{serverIdentity.protocolMinor}</span>
                </div>
                <p>Open the RCD VlogCutter web interface in Chrome, go to <strong>Settings → Devices</strong>, and generate a pairing code. Paste it below:</p>
                <label className="field">
                  <span>Pairing code</span>
                  <input
                    type="text"
                    className="mono"
                    placeholder="ABCDEFGHIJKLMNOP"
                    value={pairingCode}
                    onChange={(e) => setPairingCode(e.target.value)}
                    onKeyDown={(e) => { if (e.key === "Enter") void handlePairWithServer(); }}
                    autoFocus
                    maxLength={20}
                  />
                </label>
                <label className="field">
                  <span>This device label</span>
                  <input
                    type="text"
                    value={deviceLabel}
                    onChange={(e) => setDeviceLabel(e.target.value)}
                    placeholder="MacBook Pro"
                  />
                </label>
                <div className="modal-actions">
                  <button className="button secondary" onClick={() => setPairingWizardStep("endpoint")}>Back</button>
                  <button className="button primary" disabled={busy || !pairingCode.trim()} onClick={() => void handlePairWithServer()}>Pair with server</button>
                </div>
              </div>
            )}

            {pairingWizardStep === "connecting" && (
              <div className="wizard-step">
                <p>Connecting to server…</p>
              </div>
            )}
          </section>
        </div>
      )}

      {/* ── Baseline pairing wizard ────────────────────────────────── */}
      {showBaselineWizard && (
        <div className="modal-backdrop pairing-backdrop" role="presentation" onKeyDown={(event) => { if (event.key === "Escape") handleCancelBaselineWizard(); }}>
          <section className="modal baseline-modal" role="dialog" aria-modal="true" aria-labelledby="baseline-title">
            <span className="eyebrow">Pair project with server</span>
            <div className="settings-modal-heading">
              <h2 id="baseline-title">
                {baselineStep === "root_mapping" && "Step 1: Map media paths"}
                {baselineStep === "preview" && "Step 2: Review baseline preview"}
                {baselineStep === "confirming" && "Uploading baseline…"}
                {baselineStep === "complete" && "Project paired!"}
                {baselineStep === "error" && "Pairing issue"}
              </h2>
              <button className="text-button" onClick={handleCancelBaselineWizard}>Cancel <kbd>Esc</kbd></button>
            </div>

            {baselineError && <p className="error-message" role="alert">{baselineError}</p>}

            {/* Step 1: Root mapping */}
            {baselineStep === "root_mapping" && pathAnalysis && (
              <div className="wizard-step">
                <p>Your project has <strong>{pathAnalysis.absolutePathCount}</strong> assets with absolute paths. Map each source folder to a logical storage root that the server can recognize.</p>
                {pathAnalysis.commonPrefixes.length === 0 ? (
                  <p className="quiet">No absolute-path assets found — skipping root mapping.</p>
                ) : (
                  <div className="root-mapping-table">
                    <div className="root-mapping-header">
                      <span>Local path</span>
                      <span>Assets</span>
                      <span>Storage root ID</span>
                    </div>
                    {rootMappings.map((mapping, i) => (
                      <div className="root-mapping-row" key={i}>
                        <span className="mono" title={mapping.localPrefix}>{mapping.localPrefix}</span>
                        <span>{pathAnalysis.commonPrefixes[i]?.assetCount ?? "?"}</span>
                        <span className="mono">{mapping.storageRootId}</span>
                      </div>
                    ))}
                  </div>
                )}
                <p className="quiet">No original media will be uploaded during baseline pairing.</p>
                <div className="modal-actions">
                  <button className="button secondary" onClick={handleCancelBaselineWizard}>Cancel</button>
                  <button className="button primary" disabled={busy} onClick={() => void handleProceedToPreview()}>Preview baseline</button>
                </div>
              </div>
            )}

            {/* Step 2: Baseline preview */}
            {baselineStep === "preview" && baselinePreview && (
              <div className="wizard-step">
                <p>Server preview of <strong>{baselinePreview.preview.projectName}</strong>:</p>
                <div className="baseline-stats">
                  <div className="baseline-stat">
                    <span className="baseline-stat-value">{baselinePreview.preview.assetCount}</span>
                    <span className="baseline-stat-label">Assets</span>
                  </div>
                  <div className="baseline-stat">
                    <span className="baseline-stat-value">{baselinePreview.preview.operationCount}</span>
                    <span className="baseline-stat-label">Operations</span>
                  </div>
                  <div className="baseline-stat">
                    <span className="baseline-stat-value">{baselinePreview.preview.conflictCount}</span>
                    <span className="baseline-stat-label">Conflicts</span>
                  </div>
                  <div className="baseline-stat">
                    <span className="baseline-stat-value">{baselinePreview.preview.rootMappingCount}</span>
                    <span className="baseline-stat-label">Root mappings</span>
                  </div>
                  <div className="baseline-stat">
                    <span className="baseline-stat-value">{baselinePreview.preview.linkedMediaCount}</span>
                    <span className="baseline-stat-label">Linked media</span>
                  </div>
                </div>
                <p className="quiet">Baseline digest: <span className="mono">{baselinePreview.digest.slice(0, 16)}…</span></p>
                <p className="quiet">This operation will NOT upload any original media files. Only project metadata and manifests are transferred.</p>
                <div className="modal-actions">
                  <button className="button secondary" onClick={() => setBaselineStep("root_mapping")}>Back</button>
                  <button className="button primary" disabled={busy} onClick={() => void handleConfirmBaseline()}>Confirm &amp; upload baseline</button>
                </div>
              </div>
            )}

            {/* Step 3: Confirming */}
            {baselineStep === "confirming" && (
              <div className="wizard-step">
                <p>Uploading baseline to server…</p>
              </div>
            )}

            {/* Step 4: Complete */}
            {baselineStep === "complete" && (
              <div className="wizard-step">
                <p className="success">Project is now paired with the server!</p>
                <p>The server has a replica of your project. Background sync will begin automatically.</p>
                <div className="modal-actions">
                  <button className="button primary" autoFocus onClick={() => { setShowBaselineWizard(false); setBaselineStep("root_mapping"); setPathAnalysis(null); setRootMappings([]); setBaselinePreview(null); }}>Done</button>
                </div>
              </div>
            )}

            {/* Error state */}
            {baselineStep === "error" && (
              <div className="wizard-step">
                <p>The baseline operation did not complete. You can retry safely — no data has been lost.</p>
                <div className="modal-actions">
                  <button className="button secondary" onClick={handleCancelBaselineWizard}>Abandon</button>
                  <button className="button primary" onClick={() => void handleRetryBaseline()}>Retry</button>
                </div>
              </div>
            )}
          </section>
        </div>
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
          verifyCopies={verifyImportCopies}
          makeProxies={makeImportProxies}
          onSelectRoot={(id) => void refreshImportPlan(id)}
          onCreateRoot={(name) => void createImportArchive(name)}
          onLocateRoot={(root) => void locateImportArchive(root)}
          onVerifyCopies={setVerifyImportCopies}
          onMakeProxies={setMakeImportProxies}
          onImport={() => void runSmartImport()}
          onCancel={() => void api.cancelImport()}
          onClose={closeImportDialog}
        />
      )}
      {showImportHistory && (
        <ImportHistoryDialog
          sessions={importSessions}
          detail={importSessionDetail}
          preview={importRollback}
          onOpen={(id) => void openImportSession(id)}
          onPreview={(id) => void previewImportRollback(id)}
          onUndo={(id) => void rollbackImportSession(id)}
          onClose={() => setShowImportHistory(false)}
        />
      )}

    </main>
  );
}

export default App;
