import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { StatusBadge } from "@rcdvlog/review-ui";
import { api } from "./api";
import type { AnalysisReadiness, AnalysisStage, Asset, AssetAnalysisIndexEntry, AssetAnalysisView, AssetPatch, AutoplayMode, BaselineImportResult, BaselinePreviewResult, ClipTranscript, DesktopSettings, ModelInstallProgress, ModelStatus, PathAnalysis, Project, ProjectLibrary, ProjectLocation, ProjectPairingStatus, RootMappingEntry, ServerConnectionStatus, ServerVersionInfo, SyncStatus, TranscriptStatus } from "./types";
import { orderAssets, moveSelection as moveSelectionInOrder } from "./review/order";
import { resolveAction } from "./review/keymap";
import { CalibrationPanel } from "./CalibrationPanel";
import { EditPlanPanel } from "./EditPlanPanel";
import { formatBytes, transcriptStatusLabels } from "./review/summaries";

interface OperationProgress {
  kind: "proxy" | "thumbnail" | "analysis";
  completed: number;
  total: number;
  completedWorkUnits: number;
  totalWorkUnits: number;
  workerCount: number;
  estimatedRemainingSeconds: number | null;
  /** Analysis only: which stage of the chain is running, and what it has spent. */
  stage?: AnalysisStage;
  stageIndex?: number;
  stageCount?: number;
  costUsd?: number;
}

const analysisStageLabels: Record<AnalysisStage, string> = {
  proxies: "Making proxies",
  transcripts: "Transcribing",
  analysis: "Analyzing clips",
};

/** The clip's display name: what analysis called it, else its filename. */
function clipTitle(asset: Asset) {
  return asset.title.trim() || asset.filename;
}

function formatCost(usd: number) {
  // A non-finite value reaches here when the budget field is empty or garbage;
  // rendering "$NaN" beside a spend figure is worse than saying nothing.
  if (!Number.isFinite(usd)) return "the run budget";
  if (usd <= 0) return "$0.00";
  if (usd < 0.01) return "<$0.01";
  return `$${usd.toFixed(2)}`;
}

function formatTimecode(ms: number) {
  const total = Math.max(0, Math.round(ms / 1000));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

const autoplayOptions: Array<{ value: AutoplayMode; label: string; detail: string }> = [
  { value: "never", label: "Never", detail: "Keep each clip paused until you press Space." },
  { value: "proxyAvailable", label: "Only when proxy is available", detail: "Autoplay lightweight review media; keep camera originals paused." },
  { value: "always", label: "Always", detail: "Autoplay whichever version is available, including camera originals." },
];

const chronologyZoomMin = 44;
const chronologyZoomMax = 116;
const chronologyZoomStep = 12;
const chronologyZoomDefault = 68;

function formatDuration(seconds: number) {
  const safe = Number.isFinite(seconds) ? Math.max(0, seconds) : 0;
  const minutes = Math.floor(safe / 60);
  const remainder = Math.floor(safe % 60);
  return `${minutes}:${remainder.toString().padStart(2, "0")}`;
}

function formatEta(seconds: number | null) {
  if (seconds === null || !Number.isFinite(seconds)) return "Estimating…";
  const rounded = Math.max(0, Math.ceil(seconds));
  if (rounded < 60) return `${rounded}s remaining`;
  const minutes = Math.ceil(rounded / 60);
  if (minutes < 60) return `${minutes}m remaining`;
  const hours = Math.floor(minutes / 60);
  const remainder = minutes % 60;
  return `${hours}h ${remainder}m remaining`;
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

function formatDate(value: string | null) {
  if (!value) return "No camera time";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(date);
}

function patchFor(asset: Asset): AssetPatch {
  return {
    notes: asset.notes,
    labels: asset.labels,
    removed: asset.removed,
    rotationDegrees: asset.rotationDegrees,
  };
}

function rotationAfter(rotationDegrees: number, addition: number) {
  return ((rotationDegrees + addition) % 360 + 360) % 360;
}

function portraitRotation(asset: Asset) {
  return rotationAfter(asset.defaultRotationDegrees, 270);
}

function isRawOrientationProxy(asset: Asset) {
  return asset.proxyRepresentation === "raw_v3";
}

function isRawOrientationThumbnail(asset: Asset) {
  return Boolean(asset.thumbnailPath?.endsWith(".v2.jpg"));
}

function renderedRotation(asset: Asset, representationIsRaw: boolean) {
  return rotationAfter(
    asset.rotationDegrees,
    representationIsRaw ? 0 : -asset.metadataRotationDegrees,
  );
}

function assetContextKey(asset: Asset) {
  return [asset.phase, asset.location, asset.timezone, asset.storyBeat]
    .map((value) => value?.trim() || "")
    .join("\u0000");
}

function App() {
  // D01-T015/T016/T023: the calibration and edit-plan drawers. Closed by
  // default so the review queue stays the primary surface.
  const [chronologyDrawer, setChronologyDrawer] = useState<"none" | "calibration" | "editPlan">("none");
  const [projects, setProjects] = useState<Project[]>([]);
  const [projectId, setProjectId] = useState("");
  const [assets, setAssets] = useState<Asset[]>([]);
  const [selectedId, setSelectedId] = useState("");
  const [query, setQuery] = useState("");
  const [showRemoved, setShowRemoved] = useState(false);
  const [labelDraft, setLabelDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [operationProgress, setOperationProgress] = useState<OperationProgress | null>(null);
  const [message, setMessage] = useState("Loading catalog…");
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
  });
  const [isMuted, setIsMuted] = useState(true);
  const [isPlaying, setIsPlaying] = useState(false);
  const [playbackSeconds, setPlaybackSeconds] = useState(0);
  const [mediaDuration, setMediaDuration] = useState(0);
  const [isBuffering, setIsBuffering] = useState(false);
  const [isViewerFullscreen, setIsViewerFullscreen] = useState(false);
  const [offsetMinutes, setOffsetMinutes] = useState(0);
  const [offsetLabel, setOffsetLabel] = useState("Camera time change");
  const [chronologyZoom, setChronologyZoom] = useState(chronologyZoomDefault);

  // ── Clip analysis state ────────────────────────────────────────────
  const [analysisIndex, setAnalysisIndex] = useState<Record<string, AssetAnalysisIndexEntry>>({});
  const [readiness, setReadiness] = useState<AnalysisReadiness | null>(null);
  const [showAnalysisDialog, setShowAnalysisDialog] = useState(false);
  const [budgetInput, setBudgetInput] = useState("1.00");
  const [forceReanalyze, setForceReanalyze] = useState(false);
  const [showUnusableOnly, setShowUnusableOnly] = useState(false);
  const [transcript, setTranscript] = useState<ClipTranscript | null>(null);
  const [observation, setObservation] = useState<AssetAnalysisView | null>(null);
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
  const orientationQueueRef = useRef<string[]>([]);
  const orientationActiveRef = useRef(new Set<string>());

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
    return [...grouped.entries()].slice(0, 5);
  }, [activeAssets]);

  const contextRuns = useMemo(() => {
    const runs: Array<{ start: number; end: number; asset: Asset }> = [];
    activeAssets.forEach((asset, index) => {
      const previous = runs.at(-1);
      if (previous && assetContextKey(previous.asset) === assetContextKey(asset)) {
        previous.end = index;
      } else {
        runs.push({ start: index, end: index, asset });
      }
    });
    return runs;
  }, [activeAssets]);

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

  const refreshAssets = useCallback(async (activeProjectId: string) => {
    if (!activeProjectId) {
      setAssets([]);
      return;
    }
    const result = await api.listAssets(activeProjectId);
    setAssets(result);
    setSelectedId((current) =>
      result.some((asset) => asset.id === current) ? current : result[0]?.id || "",
    );
    setMessage(result.length ? `${result.length} clips ready` : "Choose a media folder to begin");
  }, []);

  useEffect(() => {
    Promise.all([refreshProjects(), refreshLibrary(), api.prerequisiteStatus(), api.desktopSettings()])
      .then(([, , tools, desktopSettings]) => {
        setSettings(desktopSettings);
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
    refreshAssets(projectId).catch((reason) => setError(String(reason)));
  }, [projectId, refreshAssets]);

  // ── Clip analysis loaders ─────────────────────────────────────────

  const refreshAnalysisIndex = useCallback(async (activeProjectId: string) => {
    if (!activeProjectId) {
      setAnalysisIndex({});
      return;
    }
    const entries = await api.projectAnalysisIndex(activeProjectId);
    setAnalysisIndex(Object.fromEntries(entries.map((entry) => [entry.assetId, entry])));
  }, []);

  const refreshReadiness = useCallback(async (activeProjectId: string) => {
    if (!activeProjectId) {
      setReadiness(null);
      return;
    }
    setReadiness(await api.analysisReadiness(activeProjectId));
  }, []);

  useEffect(() => {
    void refreshAnalysisIndex(projectId).catch(() => setAnalysisIndex({}));
    void refreshReadiness(projectId).catch(() => setReadiness(null));
  }, [projectId, refreshAnalysisIndex, refreshReadiness]);

  useEffect(() => {
    void api.modelStatus().then(setModels).catch(() => setModels([]));
    void api.openRouterKeyPresent().then(setKeyPresent).catch(() => setKeyPresent(false));
  }, []);

  // Per-clip detail is fetched on selection rather than for the whole project:
  // segment lists and OCR are large and only one clip is on screen.
  useEffect(() => {
    if (!selectedId) {
      setTranscript(null);
      setObservation(null);
      return;
    }
    let cancelled = false;
    void Promise.all([api.assetTranscript(selectedId), api.assetAnalysis(selectedId)])
      .then(([loadedTranscript, loadedObservation]) => {
        if (cancelled) return;
        setTranscript(loadedTranscript);
        setObservation(loadedObservation);
      })
      .catch(() => {
        if (cancelled) return;
        setTranscript(null);
        setObservation(null);
      });
    return () => {
      cancelled = true;
    };
  }, [selectedId, assets]);

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
          setAssets((current) => current.map((candidate) => candidate.id === updated.id ? updated : candidate));
        })
        .catch((reason) => {
          if (selectedIdRef.current === assetId) setError(String(reason));
        })
        .finally(() => {
          orientationActiveRef.current.delete(assetId);
          pumpOrientationQueue();
        });
    }
  }, []);

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
            setAssets((current) => current.map((asset) => asset.id === progress.asset?.id ? progress.asset : asset));
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
            setAssets((current) => current.map((asset) => asset.id === progress.asset?.id ? progress.asset : asset));
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
            setAssets((current) => current.map((asset) => asset.id === progress.asset?.id ? progress.asset : asset));
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
      ]);
      if (disposed) listeners.forEach((dispose) => dispose());
      else unlisten.push(...listeners);
    };
    void attach();
    return () => {
      disposed = true;
      unlisten.forEach((dispose) => dispose());
    };
  }, []);

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

  const replaceAsset = (updated: Asset) => {
    setAssets((current) =>
      current
        .map((asset) => (asset.id === updated.id ? updated : asset))
        .sort((a, b) =>
          (a.bestTime ?? `z${a.filename}`).localeCompare(b.bestTime ?? `z${b.filename}`),
        ),
    );
  };

  /// Commits an edited title/description. Separate from `persistAsset` because
  /// these are not part of the full-replacement `AssetPatch` contract.
  const saveClipSummary = useCallback(async (asset: Asset) => {
    try {
      replaceAsset(await api.setClipSummary(asset.id, asset.title, asset.description));
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  const persistAsset = useCallback(
    async (asset: Asset, changes: Partial<AssetPatch>) => {
      const optimistic = { ...asset, ...changes };
      replaceAsset(optimistic);
      try {
        const updated = await api.updateAsset(asset.id, { ...patchFor(asset), ...changes });
        replaceAsset(updated);
        setMessage("Clip saved");
      } catch (reason) {
        replaceAsset(asset);
        setError(String(reason));
      }
    },
    [],
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

  const handleNotesKeyDown = (event: ReactKeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      const editing = notesEditingRef.current;
      if (selected && editing?.assetId === selected.id) {
        editing.cancelOnBlur = true;
        replaceAsset({ ...selected, notes: editing.savedNotes });
      }
      event.currentTarget.blur();
      return;
    }

    if (event.key !== "Enter" || event.nativeEvent.isComposing) return;

    event.preventDefault();
    event.stopPropagation();
    if (event.repeat || !selected) return;

    if (event.metaKey) {
      const textarea = event.currentTarget;
      const selectionStart = textarea.selectionStart;
      const selectionEnd = textarea.selectionEnd;
      const notes = `${selected.notes.slice(0, selectionStart)}\n${selected.notes.slice(selectionEnd)}`;
      replaceAsset({ ...selected, notes });
      window.requestAnimationFrame(() => textarea.setSelectionRange(selectionStart + 1, selectionStart + 1));
      return;
    }

    notesEditingRef.current = null;
    event.currentTarget.blur();
    void persistAsset(selected, { notes: selected.notes });

    if (event.shiftKey) {
      moveSelection(1);
    }
  };

  const handleNotesBlur = () => {
    const editing = notesEditingRef.current;
    notesEditingRef.current = null;
    if (!selected || editing?.assetId !== selected.id || editing.cancelOnBlur) return;
    void persistAsset(selected, { notes: selected.notes });
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
        setShowMediaMenu(false);
        setShowShortcuts(false);
        setShowSettings(false);
        setShowAnalysisDialog(false);
        if (projects.length > 0) setShowCreate(false);
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
            setChronologyZoom((current) =>
              reviewAction.delta < 0
                ? Math.max(chronologyZoomMin, current - chronologyZoomStep)
                : Math.min(chronologyZoomMax, current + chronologyZoomStep),
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
          case "toggleFocus":
          case "toggleOverlays":
          case "toggleShelf":
          case "toggleRibbon":
          case "ribbonOverlay":
          case "setMode":
            // Presentations and modes arrive in Tasks 9 and 10. Returning here
            // rather than falling through keeps these keys inert instead of
            // reaching the handlers below and firing something unrelated.
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
  }, [busy, centerSelectedInChronology, moveSelection, projectId, projects.length, selected, toggleRemoved]);

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
      setAssets([]);
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
      if (!path) {
        if (!api.isDesktop()) setMessage("Opening a catalog is available in the desktop build");
        return;
      }
      setBusy(true);
      setMessage("Opening catalog…");
      const loaded = await api.loadCatalog(path);
      setProjects(loaded.projects);
      setProjectId(loaded.projects[0]?.id ?? "");
      setAssets([]);
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
    if (!path) {
      if (!api.isDesktop()) setMessage("Folder import is available in the desktop build");
      return;
    }
    setBusy(true);
    setMessage("Reading media metadata…");
    try {
      const report = await api.ingestFolder(projectId, path);
      await Promise.all([refreshAssets(projectId), refreshProjects()]);
      setMessage(
        `Imported ${report.imported}; ${report.duplicates} already present${
          report.failures.length ? `; ${report.failures.length} failed` : ""
        }`,
      );
      if (report.failures.length) setError(report.failures[0].error);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
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
      await refreshAssets(projectId);
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
      await Promise.all([refreshReadiness(projectId), api.modelStatus().then(setModels), api.openRouterKeyPresent().then(setKeyPresent)]);
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
      await Promise.all([refreshAssets(projectId), refreshAnalysisIndex(projectId), refreshReadiness(projectId)]);
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
      if (projectId) await refreshReadiness(projectId);
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
      if (projectId) await refreshReadiness(projectId);
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
      await refreshAssets(projectId);
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
      await refreshAssets(projectId);
      setMessage(`Reordered ${selected.camera} from ${selected.filename}`);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  const bufferedMedia = useMemo(() => {
    const selectedIndex = filteredAssets.findIndex((asset) => asset.id === selectedId);
    if (selectedIndex < 0) return [];
    return [-1, 0, 1]
      .map((offset) => ({ asset: filteredAssets[selectedIndex + offset], offset }))
      .filter((entry): entry is { asset: Asset; offset: number } => Boolean(entry.asset?.orientationProbed && entry.asset.available))
      .map(({ asset, offset }) => {
        const url = api.mediaUrl(asset);
        const usingProxy = Boolean(asset.proxyAvailable && asset.proxyPath);
        const rotation = renderedRotation(asset, usingProxy && isRawOrientationProxy(asset));
        const displayIsQuarterTurn = asset.rotationDegrees % 180 !== 0;
        const displayAspect = (displayIsQuarterTurn ? asset.height / asset.width : asset.width / asset.height) || 16 / 9;
        return {
          asset,
          offset,
          active: asset.id === selectedId,
          url,
          usingProxy,
          rotation,
          displayAspect,
          posterUrl: api.thumbnailUrl(asset),
        };
      })
      .filter((entry) => Boolean(entry.url));
  }, [filteredAssets, selectedId]);
  const activeMedia = bufferedMedia.find((entry) => entry.active) ?? null;
  const mediaUrl = activeMedia?.url ?? null;
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

  // ── Sync helpers ────────────────────────────────────────────────────

  function syncStateLabel(state: string): string {
    switch (state) {
      case "unpaired": return "Unpaired";
      case "offline": return "Offline";
      case "connecting": return "Connecting…";
      case "syncing": return "Syncing…";
      case "synced": return "Synced";
      case "attention": return "Needs attention";
      case "incompatible": return "Incompatible";
      case "credential_revoked": return "Credential revoked";
      default: return state;
    }
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

  const seekMaximum = Math.max(0, mediaDuration || selected?.durationSeconds || 0);
  const seekProgress = seekMaximum > 0 ? Math.min(100, (playbackSeconds / seekMaximum) * 100) : 0;

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="wordmark" aria-label="RCD VlogCutter">
          <span className="wordmark-mark">RCD</span>
          <span>VlogCutter</span>
        </div>
        <label className="project-switcher">
          <span className="sr-only">Current project</span>
          <select value={projectId} onChange={(event) => setProjectId(event.target.value)}>
            {projects.map((item) => (
              <option value={item.id} key={item.id}>{item.name}</option>
            ))}
          </select>
        </label>
        {api.isDesktop() && <button className="text-button" disabled={busy} onClick={openCatalog} aria-keyshortcuts="Meta+O">Open catalog</button>}
        <button className="text-button" onClick={() => setShowSettings(true)} aria-keyshortcuts="Meta+,">Settings <kbd>⌘,</kbd></button>
        <button className="text-button" onClick={() => setShowCreate(true)} aria-keyshortcuts="Meta+N">New project</button>
        {api.isDesktop() && serverStatus?.paired && projectId && (
          <button className="text-button" disabled={busy} onClick={() => void handleStartBaselineWizard(projectId)}>
            Pair with server
          </button>
        )}
        {syncStatus && syncStatus.state !== "unpaired" && (
          <button
            className={`sync-indicator sync-${syncStatus.state}`}
            onClick={() => setShowSyncDiagnostics(true)}
            title={`Sync: ${syncStatus.state}${syncStatus.lastError ? ` — ${syncStatus.lastError}` : ""}`}
          >
            <span className={`sync-dot sync-dot-${syncStatus.state}`} />
            <span className="sync-label">{syncStateLabel(syncStatus.state)}</span>
          </button>
        )}
        <div className="topbar-spacer" />
        <div className="session-readout">
          <span>{labeledCount}/{activeAssets.length} labeled</span>
          <span>{removedCount} removed</span>
        </div>
        <div className="media-split">
          <button className="button secondary media-split-main" disabled={busy || missingProxyCount === 0} onClick={makeMissingProxies} aria-keyshortcuts="Shift+P">
            {missingProxyCount ? `Make ${missingProxyCount} proxies` : "Proxies ready"}
          </button>
          <button
            className="button secondary media-split-toggle"
            disabled={busy || !projectId}
            onClick={() => setShowMediaMenu((current) => !current)}
            aria-label="More media generation actions"
            aria-expanded={showMediaMenu}
          >▾</button>
          {showMediaMenu && (
            <div className="media-menu">
              <button disabled={busy || missingThumbnailCount === 0} onClick={makeMissingThumbnails} aria-keyshortcuts="Shift+G">
                <span>{missingThumbnailCount ? `Generate ${missingThumbnailCount} thumbnails` : "Thumbnails ready"}</span>
                <kbd>⇧G</kbd>
                <small>Quality 1 · frame at 20%</small>
              </button>
              <button disabled={busy || !projectId} onClick={openAnalysisDialog} aria-keyshortcuts="A">
                <span>{readiness?.missingAnalyses ? `Analyze ${readiness.missingAnalyses} clips` : "Analysis up to date"}</span>
                <kbd>A</kbd>
                <small>Proxies, speech, then cloud vision</small>
              </button>
            </div>
          )}
        </div>
        <button className="button primary" disabled={busy || !projectId} onClick={importFolder} aria-keyshortcuts="M">
          Add media <kbd>M</kbd>
        </button>
        <button className="shortcut-help-button" onClick={() => setShowShortcuts(true)} aria-label="Keyboard shortcuts" aria-keyshortcuts="?">?</button>
      </header>

      <section className="status-line" aria-live="polite">
        <span className={busy ? "pulse" : "status-dot"} />
        {operationProgress ? (
          <div className="operation-progress">
            <span>
              {operationProgress.kind === "analysis" && operationProgress.stage
                ? `${analysisStageLabels[operationProgress.stage]} (step ${(operationProgress.stageIndex ?? 0) + 1} of ${operationProgress.stageCount ?? 3})…`
                : operationProgress.kind === "thumbnail"
                  ? "Generating thumbnails…"
                  : "Generating proxies…"}
            </span>
            <div
              className="progress-track"
              role="progressbar"
              aria-valuemin={0}
              aria-valuemax={operationProgress.totalWorkUnits || operationProgress.total}
              aria-valuenow={operationProgress.totalWorkUnits ? operationProgress.completedWorkUnits : operationProgress.completed}
            >
              <span style={{ width: `${operationProgress.totalWorkUnits
                ? (operationProgress.completedWorkUnits / operationProgress.totalWorkUnits) * 100
                : operationProgress.total
                  ? (operationProgress.completed / operationProgress.total) * 100
                  : 0}%` }} />
            </div>
            <output>{operationProgress.completed}/{operationProgress.total}</output>
            <span className="progress-eta">
              {formatEta(operationProgress.estimatedRemainingSeconds)} · {operationProgress.workerCount} worker{operationProgress.workerCount === 1 ? "" : "s"}
              {operationProgress.kind === "analysis" ? ` · ${formatCost(operationProgress.costUsd ?? 0)} spent` : ""}
            </span>
            {operationProgress.kind === "analysis" && (
              <button className="button ghost progress-cancel" onClick={stopAnalysis}>Cancel</button>
            )}
          </div>
        ) : <span>{message}</span>}
        {error && <span className="error-message">{error}</span>}
      </section>

      <div className="workbench">
        <aside className="queue-panel">
          <div className="panel-heading">
            <div>
              <span className="eyebrow">{showRemoved ? "Removed clips" : "Label queue"}</span>
              <h1>{project?.name ?? "No project"}</h1>
            </div>
            <div className="queue-heading-actions">
              {unusableCount > 0 && !showRemoved && (
                <button
                  className={`queue-view-toggle ${showUnusableOnly ? "active" : ""}`}
                  onClick={() => setShowUnusableOnly((current) => !current)}
                  aria-pressed={showUnusableOnly}
                  title="Clips the analysis judged unusable"
                >{showUnusableOnly ? "All clips" : `Unusable ${unusableCount}`}</button>
              )}
              {removedCount > 0 && (
                <button
                  className={`queue-view-toggle ${showRemoved ? "active" : ""}`}
                  onClick={() => setShowRemoved((current) => !current)}
                  aria-pressed={showRemoved}
                >{showRemoved ? "Back to labels" : `Removed ${removedCount}`}</button>
              )}
              <span className="count-badge">{filteredAssets.length}</span>
            </div>
          </div>
          <label className="search-box">
            <span className="sr-only">Filter clips</span>
            <input
              id="queue-search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Filter titles, labels, notes, speech…"
            />
            <kbd>/</kbd>
          </label>
          <div className="queue-list" ref={queueRef} role="listbox" aria-label="Clips" tabIndex={0}>
            {filteredAssets.length === 0 ? (
              <div className="empty-queue">
                <strong>{showRemoved ? "No removed clips." : "No clips here yet."}</strong>
                <span>{showRemoved ? "Removed clips can be restored here." : "Add a NAS or drive folder to begin."}</span>
              </div>
            ) : filteredAssets.map((asset, index) => {
              const thumbnailUrl = api.thumbnailUrl(asset);
              return (
                <button
                  className={`queue-item ${asset.id === selectedId ? "selected" : ""}`}
                  key={asset.id}
                  onClick={() => setSelectedId(asset.id)}
                  role="option"
                  aria-selected={asset.id === selectedId}
                >
                  <span className={`label-pip ${asset.labels.length ? "labeled" : ""}`} aria-hidden="true" />
                  <span
                    className={`queue-thumbnail ${renderedRotation(asset, isRawOrientationThumbnail(asset)) % 180 !== 0 ? "quarter-turn" : ""}`}
                    style={{ "--thumbnail-rotation": `${renderedRotation(asset, isRawOrientationThumbnail(asset))}deg` } as CSSProperties}
                  >
                    {thumbnailUrl ? <img src={thumbnailUrl} alt="" loading="lazy" /> : <span className="thumbnail-empty">Q0</span>}
                    <span className="queue-index">{String(index + 1).padStart(3, "0")}</span>
                  </span>
                  <span className="queue-main">
                    <strong>{clipTitle(asset)}</strong>
                    {/* The filename stays visible but stops being the headline
                        once analysis has named the clip. The verdict sits on
                        this line, inside the flexible column, so a long reason
                        truncates instead of widening the row. */}
                    <span className="queue-meta">
                      {analysisIndex[asset.id]?.usable === false && (
                        <span className="unusable-pip" title={`Analysis flagged this clip: ${analysisIndex[asset.id]?.unusableReason || "unusable"}`}>
                          {analysisIndex[asset.id]?.unusableReason || "unusable"}
                        </span>
                      )}
                      <span>{asset.title.trim() ? `${asset.filename} · ${asset.camera}` : asset.camera}</span>
                    </span>
                  </span>
                  <span className="queue-tail">
                    <time>{formatDuration(asset.durationSeconds)}</time>
                    {asset.labels.length > 0 && <span className="label-count">{asset.labels.length} label{asset.labels.length === 1 ? "" : "s"}</span>}
                  </span>
                </button>
              );
            })}
          </div>
          <div className="shortcut-strip" aria-label="Keyboard shortcuts">
            <span><kbd>J</kbd><kbd>K</kbd> move</span>
            <span><kbd>Space</kbd> play</span>
            <span><kbd>L</kbd> label</span>
            <span><kbd>R</kbd> remove</span>
          </div>
        </aside>

        <section className="viewer-panel">
          <div ref={viewerStageRef} className={`viewer-stage ${isPlaying ? "is-playing" : "is-paused"}`}>
            {selected ? (
              !selected.orientationProbed && selected.originalAvailable ? (
                <div className="offline-slate">
                  <strong>Reading frame orientation…</strong>
                  <span>{selected.filename}</span>
                </div>
              ) : mediaUrl ? (
                <>
                  {bufferedMedia.map((entry) => {
                    const style = {
                      "--display-aspect": String(entry.displayAspect),
                      "--inverse-display-aspect": String(1 / entry.displayAspect),
                      "--viewer-rotation": `${entry.rotation}deg`,
                    } as CSSProperties;
                    return (
                      <div
                        className={`viewer-media ${entry.active ? "active" : "preloaded"} ${entry.rotation % 180 !== 0 ? "quarter-turn" : ""}`}
                        style={style}
                        key={entry.asset.id}
                        aria-hidden={!entry.active}
                      >
                        <video
                          ref={entry.active ? videoRef : undefined}
                          data-asset-id={entry.asset.id}
                          data-cache-slot={entry.active ? "current" : entry.offset < 0 ? "previous" : "next"}
                          src={entry.url ?? undefined}
                          poster={entry.active && entry.posterUrl && isRawOrientationThumbnail(entry.asset) === (entry.usingProxy && isRawOrientationProxy(entry.asset)) ? entry.posterUrl : undefined}
                          preload={entry.active || entry.offset > 0 ? "auto" : "metadata"}
                          autoPlay={entry.active && shouldAutoplay}
                          muted={isMuted}
                          onClick={entry.active ? togglePlayback : undefined}
                          onLoadStart={() => entry.active && setIsBuffering(true)}
                          onWaiting={() => entry.active && setIsBuffering(true)}
                          onCanPlay={() => entry.active && setIsBuffering(false)}
                          onPlaying={() => entry.active && setIsBuffering(false)}
                          onPlay={() => entry.active && setIsPlaying(true)}
                          onPause={() => entry.active && setIsPlaying(false)}
                          onEnded={() => entry.active && setIsPlaying(false)}
                          onLoadedMetadata={(event) => {
                            if (!entry.active) return;
                            setMediaDuration(Number.isFinite(event.currentTarget.duration) ? event.currentTarget.duration : entry.asset.durationSeconds);
                            setPlaybackSeconds(event.currentTarget.currentTime);
                          }}
                          onDurationChange={(event) => {
                            if (entry.active && Number.isFinite(event.currentTarget.duration)) setMediaDuration(event.currentTarget.duration);
                          }}
                          onTimeUpdate={(event) => entry.active && setPlaybackSeconds(event.currentTarget.currentTime)}
                        />
                      </div>
                    );
                  })}
                  {isBuffering && <div className="viewer-buffering" role="status"><span /> Buffering</div>}
                </>
              ) : (
                <div className="offline-slate">
                  <span className="slate-time">{formatDate(selected.bestTime)}</span>
                  <strong>{selected.filename}</strong>
                  <span>{selected.proxyPath && !selected.proxyAvailable ? "Proxy is missing" : "No review proxy yet"}</span>
                  {api.isDesktop() && selected.available && (
                    <button className="button primary" disabled={busy} onClick={makeProxy}>Make proxy</button>
                  )}
                </div>
              )
            ) : (
              <div className="offline-slate">
                <strong>Choose a clip</strong>
                <span>The next frame will appear here.</span>
              </div>
            )}
            {selected && (
              <div className="viewer-overlay">
                <span>{selected.proxyAvailable ? "PROXY" : "ORIGINAL"}</span>
                <span>{selected.width}×{selected.height} · {selected.frameRate.toFixed(2)} fps</span>
              </div>
            )}
            {selected && mediaUrl && selected.orientationProbed && (
              <div className="viewer-controls" role="group" aria-label="Video playback controls">
                <button type="button" onClick={() => seekBy(-15)} aria-label="Go back 15 seconds" title="Back 15 seconds">−15</button>
                <button
                  type="button"
                  className="viewer-play"
                  onClick={togglePlayback}
                  aria-label={isPlaying ? "Pause" : "Play"}
                  aria-keyshortcuts="Space"
                  title={isPlaying ? "Pause (Space)" : "Play (Space)"}
                >{isPlaying ? "Ⅱ" : "▶"}</button>
                <button type="button" onClick={() => seekBy(15)} aria-label="Go forward 15 seconds" title="Forward 15 seconds">+15</button>
                <input
                  type="range"
                  min="0"
                  max={seekMaximum || 0}
                  step="0.01"
                  value={Math.min(playbackSeconds, seekMaximum)}
                  onChange={(event) => {
                    const seconds = Number(event.currentTarget.value);
                    if (videoRef.current) videoRef.current.currentTime = seconds;
                    setPlaybackSeconds(seconds);
                  }}
                  aria-label="Video position"
                  style={{ "--seek-progress": `${seekProgress}%` } as CSSProperties}
                />
                <output className="viewer-time" aria-live="off">
                  {formatDuration(playbackSeconds)} <span>/</span> {formatDuration(seekMaximum)}
                </output>
                <button
                  type="button"
                  className={isMuted ? "active" : ""}
                  onClick={toggleMute}
                  aria-label={isMuted ? "Unmute playback" : "Mute playback"}
                  aria-pressed={isMuted}
                  title={isMuted ? "Unmute" : "Mute"}
                >{isMuted ? "MUTED" : "AUDIO"}</button>
                <button
                  type="button"
                  onClick={() => void toggleViewerFullscreen()}
                  aria-label={isViewerFullscreen ? "Exit fullscreen" : "Enter fullscreen"}
                  aria-pressed={isViewerFullscreen}
                  title={isViewerFullscreen ? "Exit fullscreen" : "Fullscreen"}
                >{isViewerFullscreen ? "EXIT" : "FULL"}</button>
              </div>
            )}
          </div>

          <div className="transport-bar">
            <button className="transport-button" onClick={() => moveSelection(-1)} aria-label="Previous clip" aria-keyshortcuts="ArrowUp K">←</button>
            <button
              className="transport-button play"
              onClick={togglePlayback}
              aria-label="Play or pause"
              aria-keyshortcuts="Space"
            >{isPlaying ? "Ⅱ" : "▶"}</button>
            <button className="transport-button" onClick={() => moveSelection(1)} aria-label="Next clip" aria-keyshortcuts="ArrowDown J">→</button>
            <div className="transport-divider" />
            <span className="timecode">{selected ? `${formatDuration(playbackSeconds)} / ${formatDuration(seekMaximum)}` : "0:00"}</span>
            <button
              className={`transport-audio-button ${isMuted ? "muted" : ""}`}
              onClick={toggleMute}
              aria-label={isMuted ? "Unmute playback" : "Mute playback"}
              aria-pressed={isMuted}
              title={isMuted ? "Unmute playback for this session" : "Mute playback for this session"}
            >
              {isMuted ? "Muted" : "Sound on"}
            </button>
            <div className="transport-spacer" />
            {selected && (
              <button
                className="button label-action"
                onClick={() => document.querySelector<HTMLInputElement>("#label-input")?.focus()}
                aria-keyshortcuts="L"
              >
                Label clip <kbd>L</kbd>
              </button>
            )}
          </div>

          <div className="chronology">
            <div className="chronology-heading">
              <div>
                <span className="eyebrow">Story ribbon</span>
                <strong>{activeAssets.length ? `${activeAssets.length} clips in relative order` : "Relative order across sources"}</strong>
              </div>
              <div className="chronology-actions">
                <span className="chronology-position">
                  {selected && !selected.removed ? `#${activeAssets.findIndex((asset) => asset.id === selected.id) + 1} · ${formatDate(selected.bestTime)}` : "No active clip selected"}
                </span>
                <div className="zoom-control" aria-label="Story ribbon zoom">
                  <button
                    onClick={() => setChronologyZoom((current) => Math.max(chronologyZoomMin, current - chronologyZoomStep))}
                    disabled={chronologyZoom === chronologyZoomMin}
                    aria-label="Zoom story ribbon out"
                    aria-keyshortcuts="BracketLeft"
                  >−<kbd>[</kbd></button>
                  <input
                    type="range"
                    min={chronologyZoomMin}
                    max={chronologyZoomMax}
                    step={chronologyZoomStep}
                    value={chronologyZoom}
                    onChange={(event) => setChronologyZoom(Number(event.target.value))}
                    aria-label="Story ribbon zoom level"
                  />
                  <button
                    onClick={() => setChronologyZoom((current) => Math.min(chronologyZoomMax, current + chronologyZoomStep))}
                    disabled={chronologyZoom === chronologyZoomMax}
                    aria-label="Zoom story ribbon in"
                    aria-keyshortcuts="BracketRight"
                  >+<kbd>]</kbd></button>
                </div>
                <button className="center-story-button" onClick={() => centerSelectedInChronology()} aria-keyshortcuts="C">
                  Center <kbd>C</kbd>
                </button>
              </div>
            </div>
            {lanes.length > 0 ? (
              <div className="story-map">
                <div className="story-labels" aria-hidden="true">
                  <span className="context-label">Trip context</span>
                  <span className="order-label">Order</span>
                  {lanes.map(([camera]) => <span className="lane-name" title={camera} key={camera}>{camera}</span>)}
                </div>
                <div
                  className="story-scroll"
                  ref={chronologyScrollRef}
                  onWheel={(event) => {
                    if (Math.abs(event.deltaY) <= Math.abs(event.deltaX) || event.ctrlKey) return;
                    event.preventDefault();
                    event.currentTarget.scrollLeft += event.deltaY;
                  }}
                >
                  <div
                    className="story-canvas"
                    style={{
                      width: `${Math.max(chronologyZoom, activeAssets.length * chronologyZoom)}px`,
                      minWidth: "100%",
                      "--clip-step": `${chronologyZoom}px`,
                    } as React.CSSProperties}
                  >
                    <div className="context-track">
                      {contextRuns.map((run) => {
                        const primary = run.asset.phase || run.asset.location || "Context not tagged";
                        const detail = [
                          run.asset.location && run.asset.location !== primary ? run.asset.location : null,
                          run.asset.timezone,
                          run.asset.storyBeat,
                        ].filter(Boolean).join(" · ") || "Add phase, place, timezone or story beat";
                        return (
                          <div
                            className="context-segment"
                            key={`${run.start}-${assetContextKey(run.asset)}`}
                            style={{ left: run.start * chronologyZoom + 3, width: (run.end - run.start + 1) * chronologyZoom - 6 }}
                            title={`${primary} · ${detail}`}
                          >
                            <strong>{primary}</strong>
                            <span>{detail}</span>
                          </div>
                        );
                      })}
                    </div>
                    <div className="order-track" aria-hidden="true">
                      {activeAssets.map((asset, index) => (
                        <span className={asset.id === selectedId ? "active" : ""} style={{ left: index * chronologyZoom }} key={asset.id}>
                          {String(index + 1).padStart(3, "0")}
                        </span>
                      ))}
                    </div>
                    {lanes.map(([camera, cameraAssets]) => (
                      <div className="lane-track" key={camera}>
                        {cameraAssets.map((asset) => {
                          const index = activeAssets.findIndex((candidate) => candidate.id === asset.id);
                          return (
                            <button
                              key={asset.id}
                              className={`timeline-clip ${asset.offsetSeconds ? "corrected" : ""} ${asset.id === selectedId ? "active" : ""}`}
                              style={{ left: index * chronologyZoom + 4, width: chronologyZoom - 8 }}
                              onClick={() => setSelectedId(asset.id)}
                              aria-label={`Clip ${index + 1}, ${asset.filename}, ${camera}`}
                              title={`#${index + 1} · ${asset.filename} · ${formatDate(asset.bestTime)}`}
                            >
                              <span>{asset.filename.replace(/\.[^.]+$/, "")}</span>
                              {asset.labels.length > 0 && <i aria-hidden="true" />}
                            </button>
                          );
                        })}
                      </div>
                    ))}
                  </div>
                </div>
              </div>
            ) : <div className="empty-lanes">Camera lanes appear after ingest.</div>}
          </div>
        </section>

        <aside className="inspector-panel">
          {selected ? (
            <>
              <div className="inspector-title">
                <span className="eyebrow">Clip log</span>
                {/* Editable in place: a generated title is a suggestion, and an
                    edit must survive the clip being analyzed again. */}
                <input
                  className="clip-title-field"
                  value={selected.title}
                  placeholder={selected.filename}
                  aria-label="Clip title"
                  onChange={(event) => replaceAsset({ ...selected, title: event.target.value })}
                  onBlur={() => void saveClipSummary(selected)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") event.currentTarget.blur();
                  }}
                />
                <span className="inspector-filename">{selected.filename}</span>
                <span className={selected.available ? "availability online" : "availability offline"}>
                  {selected.available ? "Media available" : "Original offline"}
                </span>
              </div>

              <section className="inspector-section labels-section">
                <label htmlFor="label-input" className="section-label">Labels <kbd>L</kbd></label>
                <div className="label-entry">
                  <input
                    id="label-input"
                    value={labelDraft}
                    placeholder="Add place, action, subject…"
                    aria-keyshortcuts="L Enter"
                    onChange={(event) => setLabelDraft(event.currentTarget.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter" || event.key === ",") {
                        event.preventDefault();
                        addLabel(selected, labelDraft);
                      } else if (event.key === "Backspace" && !labelDraft && selected.labels.length > 0) {
                        removeLabel(selected, selected.labels.at(-1) ?? "");
                      }
                    }}
                  />
                  <button className="label-add-button" disabled={!labelDraft.trim()} onClick={() => addLabel(selected, labelDraft)}>Add</button>
                </div>
                <div className={`label-chips ${selected.labels.length ? "" : "empty"}`} aria-label="Clip labels">
                  {selected.labels.length ? selected.labels.map((label) => (
                    <button key={label} onClick={() => removeLabel(selected, label)} title={`Remove ${label}`}>
                      <span>{label}</span><i aria-hidden="true">×</i>
                    </button>
                  )) : <span>No labels yet. Add the most useful search terms for this clip.</span>}
                </div>
              </section>

              <section className="inspector-section orientation-section">
                <div className="section-heading-row">
                  <span className="section-label">Frame orientation</span>
                  <span>{selected.rotationDegrees === selected.defaultRotationDegrees ? "Landscape (automatic)" : `${selected.rotationDegrees}° override`}</span>
                </div>
                <div className="orientation-controls">
                  <button
                    className={`orientation-choice portrait ${selected.rotationDegrees === portraitRotation(selected) ? "active" : ""}`}
                    onClick={() => persistAsset(selected, {
                      rotationDegrees: selected.rotationDegrees === portraitRotation(selected)
                        ? selected.defaultRotationDegrees
                        : portraitRotation(selected),
                    })}
                    aria-pressed={selected.rotationDegrees === portraitRotation(selected)}
                    title="Show this clip vertically"
                  >
                    <span className="orientation-thumbnail" aria-hidden="true"><i /></span>
                    <strong>Portrait</strong>
                    <small>Show vertical</small>
                  </button>
                  <button
                    className="orientation-choice"
                    onClick={() => persistAsset(selected, { rotationDegrees: rotationAfter(selected.rotationDegrees, 180) })}
                    title="Rotate the current view 180 degrees"
                  >
                    <span className="orientation-thumbnail upside-down" aria-hidden="true"><i /></span>
                    <strong>Flip 180°</strong>
                    <small>Turn current view</small>
                  </button>
                </div>
                {selected.rotationDegrees !== selected.defaultRotationDegrees && (
                  <button
                    className="orientation-reset"
                    onClick={() => persistAsset(selected, { rotationDegrees: selected.defaultRotationDegrees })}
                  >
                    Reset automatic landscape
                  </button>
                )}
              </section>

              {observation && (
                <section className={`inspector-section observation-section ${observation.analysis.usable ? "" : "is-unusable"}`}>
                  <div className="section-heading-row">
                    <span className="section-label">Description</span>
                    <span className="section-note">
                      {observation.current ? observation.analysis.model : "Stale — proxy changed since"}
                    </span>
                  </div>
                  {!observation.analysis.usable && (
                    <p className="unusable-callout">
                      Flagged unusable{observation.analysis.unusableReason ? `: ${observation.analysis.unusableReason}` : ""}.
                      {" "}Your call — this does not remove the clip.
                    </p>
                  )}
                  <textarea
                    className="description-field"
                    value={selected.description}
                    placeholder="No description yet."
                    aria-label="Clip description"
                    onChange={(event) => replaceAsset({ ...selected, description: event.target.value })}
                    onBlur={() => void saveClipSummary(selected)}
                  />
                  <div className="observation-facts">
                    <span className="chip">{observation.analysis.cameraMovement}</span>
                    {observation.analysis.transcriptUsed && <span className="chip">speech used</span>}
                    {observation.aiLabels.map((label) => <span className="chip suggested" key={label}>{label}</span>)}
                  </div>
                  {observation.analysis.bestMomentStartMs !== null && (
                    <button
                      className="button ghost"
                      onClick={() => seekTo((observation.analysis.bestMomentStartMs ?? 0) / 1000)}
                    >
                      Jump to best moment · {formatTimecode(observation.analysis.bestMomentStartMs)}
                    </button>
                  )}
                  {observation.analysis.ocrText.length > 0 && (
                    <div className="ocr-list">
                      <span className="section-note">Text on screen</span>
                      <p>{observation.analysis.ocrText.join(" · ")}</p>
                    </div>
                  )}
                </section>
              )}

              {transcript && (
                <section className="inspector-section transcript-section">
                  <div className="section-heading-row">
                    <span className="section-label">Transcript</span>
                    <span className="section-note">
                      {transcriptStatusLabels[transcript.status]}
                      {transcript.status === "speech" ? ` · ${transcript.primaryLanguage || "unknown"}` : ""}
                    </span>
                  </div>
                  {transcript.segments.length === 0 ? (
                    <p className="transcript-empty">
                      {transcript.status === "no_speech"
                        ? "No speech in this clip."
                        : transcript.rejectReason || "Nothing was transcribed."}
                    </p>
                  ) : (
                    <ol className="transcript-list">
                      {transcript.segments.map((segment) => (
                        <li key={`${segment.startMs}-${segment.endMs}`}>
                          <button onClick={() => seekTo(segment.startMs / 1000)} title="Jump to this moment">
                            <span className="mono">{formatTimecode(segment.startMs)}</span>
                            <span>{segment.text}</span>
                          </button>
                        </li>
                      ))}
                    </ol>
                  )}
                </section>
              )}

              <section className="inspector-section notes-section">
                <label htmlFor="notes" className="section-label">Notes <kbd>N</kbd><kbd>Enter</kbd></label>
                <textarea
                  id="notes"
                  value={selected.notes}
                  placeholder="What happens here? What might this cut with?"
                  aria-keyshortcuts="Enter Shift+Enter Meta+Enter Escape"
                  onFocus={() => {
                    notesEditingRef.current = { assetId: selected.id, savedNotes: selected.notes, cancelOnBlur: false };
                  }}
                  onChange={(event) => replaceAsset({ ...selected, notes: event.target.value })}
                  onKeyDown={handleNotesKeyDown}
                  onBlur={handleNotesBlur}
                />
                <div className="notes-shortcuts" aria-label="Notes keyboard shortcuts">
                  <span><kbd>Enter</kbd> Save</span>
                  <span><kbd>⇧ Enter</kbd> Save &amp; next</span>
                  <span><kbd>⌘ Enter</kbd> New line</span>
                  <span><kbd>Esc</kbd> Cancel</span>
                </div>
              </section>

              <section className={`inspector-section remove-section ${selected.removed ? "is-removed" : ""}`}>
                <div>
                  <span className="section-label">Queue visibility</span>
                  <p>{selected.removed ? "Restore this clip to the labeling queue." : "Hide clips that are unusable or clearly accidental."}</p>
                </div>
                <button onClick={() => toggleRemoved(selected)} aria-keyshortcuts="R">
                  {selected.removed ? "Restore clip" : "Remove from labeling"} <kbd>R</kbd>
                </button>
              </section>

              <section className="inspector-section metadata-grid">
                <span className="section-label full-row">Source</span>
                <div><span>Camera</span><strong>{selected.camera}</strong></div>
                <div><span>Format</span><strong>{selected.codec.toUpperCase()} · {formatBytes(selected.sizeBytes)}</strong></div>
                <div><span>Raw time</span><strong className="mono">{formatDate(selected.capturedAtRaw)}</strong></div>
                <div><span>Best time</span><strong className="mono">{formatDate(selected.bestTime)}</strong></div>
              </section>

              <section className="inspector-section calibration-section">
                <div className="section-heading-row">
                  <span className="section-label">Clock event</span>
                  <span>From this filename onward</span>
                </div>
                <label className="field-label">Offset, minutes
                  <input type="number" value={offsetMinutes} onChange={(event) => setOffsetMinutes(Number(event.target.value))} />
                </label>
                <label className="field-label">Reason
                  <input value={offsetLabel} onChange={(event) => setOffsetLabel(event.target.value)} />
                </label>
                <button className="button calibration-button" disabled={busy} onClick={applyOffset}>
                  Apply to {selected.camera}
                </button>
              </section>
            </>
          ) : (
            <div className="empty-inspector">Select a clip to log it.</div>
          )}
        </aside>
      </div>

      {showCreate && (
        <div className="modal-backdrop" role="presentation">
          <section className="modal" role="dialog" aria-modal="true" aria-labelledby="new-project-title">
            <span className="eyebrow">Trip projects</span>
            <h2 id="new-project-title">Open or create a working set</h2>
            <div className="project-library-heading">
              <span className="section-label">Projects folder</span>
              {api.isDesktop() && <button className="text-button" disabled={busy} onClick={configureProjectsRoot}>Change…</button>}
            </div>
            <div className="project-root mono" title={library.rootPath}>{library.rootPath || "Loading…"}</div>
            {library.projects.length > 0 ? (
              <div className="project-library-list">
                {library.projects.map((location) => (
                  <button
                    className="project-library-item"
                    disabled={busy}
                    key={`${location.catalogPath}:${location.project.id}`}
                    onClick={() => openLibraryProject(location)}
                  >
                    <strong>{location.project.name}</strong>
                    <span>{location.project.assetCount} clips · {location.project.labeledCount} labeled</span>
                  </button>
                ))}
              </div>
            ) : (
              <p className="project-library-empty">No VlogCutter projects found in this folder.</p>
            )}
            <div className="modal-divider"><span>New project</span></div>
            <p>Originals stay where they are. The catalog, proxies, and thumbnails will live in this project’s folder.</p>
            <input
              autoFocus
              value={newProjectName}
              onChange={(event) => setNewProjectName(event.target.value)}
              onKeyDown={(event) => event.key === "Enter" && void createProject()}
              placeholder="e.g. Hokkaido winter 2026"
            />
            <div className="modal-actions">
              {api.isDesktop() && (
                <button className="button secondary" disabled={busy} onClick={openCatalog}>
                  Open existing project…
                </button>
              )}
              {projects.length > 0 && <button className="button secondary" onClick={() => setShowCreate(false)}>Cancel</button>}
              <button className="button primary" disabled={busy || !newProjectName.trim()} onClick={createProject}>Create project</button>
            </div>
          </section>
        </div>
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
        <div className="modal-backdrop" role="presentation">
          <section className="modal analysis-modal" role="dialog" aria-modal="true" aria-labelledby="analysis-title">
            <span className="eyebrow">Analyze project</span>
            <h2 id="analysis-title">{project?.name ?? "This project"}</h2>

            <ol className="stage-plan">
              <li>
                <strong>{readiness.missingProxies}</strong>
                <span>proxies to make</span>
                <small>Local. Both later steps read the proxy, so this runs first.</small>
              </li>
              <li>
                <strong>{readiness.missingTranscripts}</strong>
                <span>clips to transcribe</span>
                <small>Local and offline. No API key, no cost.</small>
              </li>
              <li>
                <strong>{readiness.missingAnalyses}</strong>
                <span>clips to analyze</span>
                <small>Cloud vision. This is the step that spends money.</small>
              </li>
            </ol>

            <div className="cost-projection">
              <div>
                <span>Projected cost</span>
                <strong>{formatCost(readiness.estimatedCostUsd)}</strong>
              </div>
              <p>
                {readiness.costBasis === "observed"
                  ? `Based on $${readiness.observedCostPerClipUsd.toFixed(6)} per clip already analyzed in this project.`
                  : `No clips analyzed here yet, so this uses the reference measurement of $${readiness.observedCostPerClipUsd.toFixed(6)} per clip. The first run replaces it with your own.`}
                {readiness.lastRun && readiness.lastRun.status !== "running"
                  ? ` Last run: ${readiness.lastRun.clipsAnalyzed} clips, ${formatCost(readiness.lastRun.costUsd)}.`
                  : ""}
              </p>
            </div>

            <label className="field-label">Stop this run at
              <input
                type="number"
                min="0"
                step="0.25"
                value={budgetInput}
                onChange={(event) => setBudgetInput(event.target.value)}
                aria-describedby="budget-note"
              />
            </label>
            <p id="budget-note" className="section-note">
              US dollars. The run stops once returned provider usage reaches this, keeping whatever it finished.
            </p>

            <label className="checkbox-row">
              <input type="checkbox" checked={forceReanalyze} onChange={(event) => setForceReanalyze(event.target.checked)} />
              <span>Re-analyze clips that already have a description. Your edits to titles and descriptions will be overwritten; notes and your own labels are never touched.</span>
            </label>

            {!readiness.modelsReady && (
              <p className="blocking-note">
                Speech models are not installed, so transcription would fail for every clip.
                {" "}
                <button className="text-button" onClick={() => { setShowAnalysisDialog(false); setShowSettings(true); }}>Install them in Settings</button>
              </p>
            )}
            {!readiness.apiKeyPresent && (
              <p className="blocking-note">
                No OpenRouter key, so clip analysis cannot run.
                {" "}
                <button className="text-button" onClick={() => { setShowAnalysisDialog(false); setShowSettings(true); }}>Add one in Settings</button>
              </p>
            )}

            <div className="modal-actions">
              <button className="button ghost" onClick={() => setShowAnalysisDialog(false)}>Cancel</button>
              <button
                className="button primary"
                disabled={!readiness.modelsReady || !readiness.apiKeyPresent || readiness.totalClips === 0}
                onClick={runAnalysis}
              >
                Start
              </button>
            </div>
          </section>
        </div>
      )}

      {showShortcuts && (
        <div className="modal-backdrop shortcut-backdrop" role="presentation">
          <section className="modal shortcut-modal" role="dialog" aria-modal="true" aria-labelledby="shortcut-title">
            <span className="eyebrow">Keyboard map</span>
            <div className="shortcut-modal-heading">
              <h2 id="shortcut-title">Review without leaving the keys</h2>
              <button className="text-button" onClick={() => setShowShortcuts(false)}>Close <kbd>Esc</kbd></button>
            </div>
            <div className="shortcut-map">
              <div><kbd>J</kbd><kbd>K</kbd><span>Next / previous clip</span></div>
              <div><kbd>Space</kbd><span>Play / pause</span></div>
              <div><kbd>L</kbd><span>Focus labels</span></div>
              <div><kbd>R</kbd><span>Remove / restore clip</span></div>
              <div><kbd>N</kbd><kbd>Enter</kbd><span>Focus notes</span></div>
              <div><kbd>Enter</kbd><span>Save note</span></div>
              <div><kbd>⇧ Enter</kbd><span>Save note and advance</span></div>
              <div><kbd>⌘ Enter</kbd><span>Insert a note line break</span></div>
              <div><kbd>Esc</kbd><span>Cancel note edit</span></div>
              <div><kbd>/</kbd><span>Search clips</span></div>
              <div><kbd>[</kbd><kbd>]</kbd><span>Zoom story ribbon</span></div>
              <div><kbd>C</kbd><span>Center selected clip in ribbon</span></div>
              <div><kbd>P</kbd><span>Proxy selected clip</span></div>
              <div><kbd>⇧P</kbd><span>Generate missing proxies</span></div>
              <div><kbd>⇧G</kbd><span>Generate missing thumbnails</span></div>
              <div><kbd>A</kbd><span>Analyze project</span></div>
              <div><kbd>M</kbd><span>Add media</span></div>
              <div><kbd>⌘N</kbd><kbd>⌘O</kbd><span>New / open project</span></div>
              <div><kbd>⌘,</kbd><span>Open global settings</span></div>
              <div><kbd>Tab</kbd><kbd>Enter</kbd><span>Navigate / activate every control</span></div>
              <div><kbd>?</kbd><span>Open this keyboard map</span></div>
            </div>
          </section>
        </div>
      )}
      {/* ── Sync diagnostics ─────────────────────────────────────── */}
      {showSyncDiagnostics && syncStatus && (
        <div className="modal-backdrop" role="presentation" onKeyDown={(event) => { if (event.key === "Escape") setShowSyncDiagnostics(false); }}>
          <section className="modal sync-diagnostics-modal" role="dialog" aria-modal="true" aria-labelledby="sync-diag-title">
            <span className="eyebrow">Sync status</span>
            <div className="settings-modal-heading">
              <h2 id="sync-diag-title">{syncStateLabel(syncStatus.state)}</h2>
              <button className="text-button" onClick={() => setShowSyncDiagnostics(false)}>Close</button>
            </div>
            <div className="sync-diag-grid">
              <div className="sync-diag-row">
                <span className="sync-diag-label">State</span>
                <StatusBadge
                  className={`sync-state-badge sync-state-${syncStatus.state}`}
                  label={syncStateLabel(syncStatus.state)}
                  tone={syncStatus.state === "synced" ? "positive" : syncStatus.state === "attention" ? "warning" : "neutral"}
                />
              </div>
              {syncStatus.lastSyncAt && (
                <div className="sync-diag-row">
                  <span className="sync-diag-label">Last sync</span>
                  <span>{new Date(syncStatus.lastSyncAt).toLocaleString()}</span>
                </div>
              )}
              {syncStatus.activeEndpoint && (
                <div className="sync-diag-row">
                  <span className="sync-diag-label">Endpoint</span>
                  <span className="endpoint-tag">{syncStatus.activeEndpoint}</span>
                </div>
              )}
              <div className="sync-diag-row">
                <span className="sync-diag-label">Pending (local → server)</span>
                <span>{syncStatus.localPendingOperations}</span>
              </div>
              <div className="sync-diag-row">
                <span className="sync-diag-label">Pending (server → local)</span>
                <span>{syncStatus.remotePendingOperations}</span>
              </div>
              {syncStatus.conflictCount > 0 && (
                <div className="sync-diag-section">
                  <div className="sync-diag-row sync-diag-warn">
                    <span className="sync-diag-label">Conflicts</span>
                    <span>{syncStatus.conflictCount} unresolved</span>
                  </div>
                  {syncStatus.conflictDetails?.map((detail) => (
                    <div key={detail.conflictId} className="sync-diag-conflict">
                      <div className="sync-diag-conflict-header">
                        <span className="sync-diag-label">{detail.conflictType.replace(/_/g, ' ')}</span>
                        <span className="sync-diag-label">on {detail.aggregateId}</span>
                      </div>
                      <div className="sync-diag-conflict-sides">
                        {detail.incomingOperationSummary && (
                          <div className="sync-diag-conflict-side">
                            <span className="sync-diag-side-label">Incoming (conflicting)</span>
                            <div className="sync-diag-conflict-detail">
                              <span>Node: {detail.incomingOperationSummary.originNodeId}</span>
                              {detail.incomingOperationSummary.camera && (
                                <span>Camera: {detail.incomingOperationSummary.camera}</span>
                              )}
                              {detail.incomingOperationSummary.startFilename && (
                                <span>File: {detail.incomingOperationSummary.startFilename}</span>
                              )}
                              {detail.incomingOperationSummary.offsetSeconds != null && (
                                <span>Offset: {detail.incomingOperationSummary.offsetSeconds}s</span>
                              )}
                              {detail.incomingOperationSummary.label && (
                                <span>Label: {detail.incomingOperationSummary.label}</span>
                              )}
                            </div>
                          </div>
                        )}
                        {detail.activeOperationSummary && (
                          <div className="sync-diag-conflict-side">
                            <span className="sync-diag-side-label">Active (currently applied)</span>
                            <div className="sync-diag-conflict-detail">
                              <span>Node: {detail.activeOperationSummary.originNodeId}</span>
                              {detail.activeOperationSummary.camera && (
                                <span>Camera: {detail.activeOperationSummary.camera}</span>
                              )}
                              {detail.activeOperationSummary.startFilename && (
                                <span>File: {detail.activeOperationSummary.startFilename}</span>
                              )}
                              {detail.activeOperationSummary.offsetSeconds != null && (
                                <span>Offset: {detail.activeOperationSummary.offsetSeconds}s</span>
                              )}
                              {detail.activeOperationSummary.label && (
                                <span>Label: {detail.activeOperationSummary.label}</span>
                              )}
                            </div>
                          </div>
                        )}
                      </div>
                      <div className="sync-diag-conflict-meta">
                        <span>Expected revision: {detail.expectedBaseRevision ?? "—"} → Actual: {detail.actualRevision}</span>
                      </div>
                    </div>
                  ))}
                </div>
              )}
              {syncStatus.lastError && (
                <div className="sync-diag-row sync-diag-error">
                  <span className="sync-diag-label">Last error</span>
                  <span>{syncStatus.lastError}</span>
                </div>
              )}
              {syncStatus.backoffUntil && (
                <div className="sync-diag-row">
                  <span className="sync-diag-label">Next retry</span>
                  <span>{new Date(syncStatus.backoffUntil).toLocaleString()}</span>
                </div>
              )}
            </div>
            <div className="sync-diag-actions">
              <button className="button secondary" onClick={() => { api.syncNow(syncStatus.projectId).catch(() => {}); }}>Sync now</button>
            </div>
          </section>
        </div>
      )}

      {projectId && (
        <div className="chronology-drawer">
          <nav className="chronology-drawer-tabs" aria-label="Chronology and editing">
            <button
              type="button"
              aria-pressed={chronologyDrawer === "calibration"}
              onClick={() =>
                setChronologyDrawer(chronologyDrawer === "calibration" ? "none" : "calibration")
              }
            >
              Chronology
            </button>
            <button
              type="button"
              aria-pressed={chronologyDrawer === "editPlan"}
              onClick={() =>
                setChronologyDrawer(chronologyDrawer === "editPlan" ? "none" : "editPlan")
              }
            >
              Edit plan
            </button>
          </nav>
          {chronologyDrawer === "calibration" && (
            <CalibrationPanel
              projectId={projectId}
              currentAssetId={selectedId || null}
              // A correction can reorder hundreds of clips; resume where the
              // user was rather than snapping them back to the top.
              onQueueResumed={(assetId) => setSelectedId(assetId)}
              onChronologyChanged={() => refreshAssets(projectId)}
            />
          )}
          {chronologyDrawer === "editPlan" && (
            <EditPlanPanel projectId={projectId} assets={assets} />
          )}
        </div>
      )}
    </main>
  );
}

export default App;
