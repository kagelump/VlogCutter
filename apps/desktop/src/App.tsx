import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { api } from "./api";
import type { Asset, AssetPatch, AutoplayMode, DesktopSettings, Project, ProjectLibrary, ProjectLocation, ReviewStatus } from "./types";

interface OperationProgress {
  kind: "proxy" | "thumbnail";
  completed: number;
  total: number;
  completedWorkUnits: number;
  totalWorkUnits: number;
  workerCount: number;
  estimatedRemainingSeconds: number | null;
}

const statusLabels: Record<ReviewStatus, string> = {
  unreviewed: "Unreviewed",
  reviewed: "Reviewed",
  skipped: "Skipped",
  needs_attention: "Needs attention",
};

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

function formatBytes(bytes: number) {
  if (bytes < 1_000_000_000) return `${(bytes / 1_000_000).toFixed(0)} MB`;
  return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
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
    status: asset.status,
    included: asset.included,
    sourceIn: asset.sourceIn,
    sourceOut: asset.sourceOut,
  };
}

function assetContextKey(asset: Asset) {
  return [asset.phase, asset.location, asset.timezone, asset.storyBeat]
    .map((value) => value?.trim() || "")
    .join("\u0000");
}

function App() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [projectId, setProjectId] = useState("");
  const [assets, setAssets] = useState<Asset[]>([]);
  const [selectedId, setSelectedId] = useState("");
  const [query, setQuery] = useState("");
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
  const [settings, setSettings] = useState<DesktopSettings>({ projectsRoot: "", autoplayMode: "proxyAvailable" });
  const [isMuted, setIsMuted] = useState(true);
  const [offsetMinutes, setOffsetMinutes] = useState(0);
  const [offsetLabel, setOffsetLabel] = useState("Camera time change");
  const [chronologyZoom, setChronologyZoom] = useState(chronologyZoomDefault);
  const activeOperationRef = useRef<OperationProgress["kind"] | null>(null);
  const notesEditingRef = useRef<{ assetId: string; savedNotes: string; cancelOnBlur: boolean } | null>(null);
  const chronologyScrollRef = useRef<HTMLDivElement>(null);
  const queueRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);

  const selected = assets.find((asset) => asset.id === selectedId) ?? null;
  const project = projects.find((item) => item.id === projectId) ?? null;

  const filteredAssets = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    if (!needle) return assets;
    return assets.filter((asset) =>
      [asset.filename, asset.camera, asset.notes, asset.status]
        .join(" ")
        .toLocaleLowerCase()
        .includes(needle),
    );
  }, [assets, query]);

  const lanes = useMemo(() => {
    const grouped = new Map<string, Asset[]>();
    assets.forEach((asset) => {
      grouped.set(asset.camera, [...(grouped.get(asset.camera) ?? []), asset]);
    });
    return [...grouped.entries()].slice(0, 5);
  }, [assets]);

  const contextRuns = useMemo(() => {
    const runs: Array<{ start: number; end: number; asset: Asset }> = [];
    assets.forEach((asset, index) => {
      const previous = runs.at(-1);
      if (previous && assetContextKey(previous.asset) === assetContextKey(asset)) {
        previous.end = index;
      } else {
        runs.push({ start: index, end: index, asset });
      }
    });
    return runs;
  }, [assets]);

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

  const replaceAsset = (updated: Asset) => {
    setAssets((current) =>
      current
        .map((asset) => (asset.id === updated.id ? updated : asset))
        .sort((a, b) =>
          (a.bestTime ?? `z${a.filename}`).localeCompare(b.bestTime ?? `z${b.filename}`),
        ),
    );
  };

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

  const moveSelection = useCallback(
    (delta: number) => {
      if (!filteredAssets.length) return;
      const index = Math.max(0, filteredAssets.findIndex((asset) => asset.id === selectedId));
      const next = Math.min(filteredAssets.length - 1, Math.max(0, index + delta));
      setSelectedId(filteredAssets[next].id);
    },
    [filteredAssets, selectedId],
  );

  const centerSelectedInChronology = useCallback((behavior: ScrollBehavior = "smooth") => {
    const scroll = chronologyScrollRef.current;
    const index = assets.findIndex((asset) => asset.id === selectedId);
    if (!scroll || index < 0) return;
    const clipCenter = index * chronologyZoom + chronologyZoom / 2;
    scroll.scrollTo({ left: Math.max(0, clipCenter - scroll.clientWidth / 2), behavior });
  }, [assets, chronologyZoom, selectedId]);

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

    // Keep the browser's native textarea insertion so the newline lands at the cursor.
    if (event.metaKey) return;

    event.preventDefault();
    if (event.repeat || !selected) return;

    if (event.shiftKey) {
      event.currentTarget.blur();
      moveSelection(1);
    } else {
      if (notesEditingRef.current?.assetId === selected.id) {
        notesEditingRef.current.savedNotes = selected.notes;
      }
      void persistAsset(selected, { notes: selected.notes });
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
        if (projects.length > 0) setShowCreate(false);
        return;
      }
      const tag = (event.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      if (event.key === "?") {
        event.preventDefault();
        setShowShortcuts(true);
      } else if (event.key === "/") {
        event.preventDefault();
        document.querySelector<HTMLInputElement>("#queue-search")?.focus();
      } else if (event.key === "ArrowDown" || key === "j") {
        event.preventDefault();
        moveSelection(1);
      } else if (event.key === "ArrowUp" || key === "k") {
        event.preventDefault();
        moveSelection(-1);
      } else if (event.code === "Space") {
        event.preventDefault();
        if (videoRef.current?.paused) void videoRef.current.play();
        else videoRef.current?.pause();
      } else if (selected && key === "r") {
        void persistAsset(selected, { status: "reviewed" });
      } else if (selected && key === "u") {
        void persistAsset(selected, { status: "unreviewed" });
      } else if (selected && key === "s") {
        void persistAsset(selected, { status: "skipped" });
      } else if (selected && key === "a") {
        void persistAsset(selected, { status: "needs_attention" });
      } else if (selected && key === "e") {
        void persistAsset(selected, { included: !selected.included });
      } else if (selected && key === "i" && videoRef.current) {
        void persistAsset(selected, { sourceIn: Math.min(videoRef.current.currentTime, selected.sourceOut) });
      } else if (selected && key === "o" && videoRef.current) {
        void persistAsset(selected, { sourceOut: Math.max(videoRef.current.currentTime, selected.sourceIn) });
      } else if (key === "n") {
        event.preventDefault();
        document.querySelector<HTMLTextAreaElement>("#notes")?.focus();
      } else if (key === "[") {
        event.preventDefault();
        setChronologyZoom((current) => Math.max(chronologyZoomMin, current - chronologyZoomStep));
      } else if (key === "]") {
        event.preventDefault();
        setChronologyZoom((current) => Math.min(chronologyZoomMax, current + chronologyZoomStep));
      } else if (key === "c") {
        event.preventDefault();
        centerSelectedInChronology();
      } else if (key === "p" && event.shiftKey) {
        void makeMissingProxies();
      } else if (key === "p") {
        void makeProxy();
      } else if (key === "t") {
        void makeMissingThumbnails();
      } else if (key === "m") {
        void importFolder();
      } else if (key === "x") {
        void exportCut();
      }
    };
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [busy, centerSelectedInChronology, moveSelection, persistAsset, projectId, projects.length, selected]);

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

  async function exportCut() {
    if (!projectId) return;
    setBusy(true);
    setError("");
    try {
      const path = await api.exportOtio(projectId);
      setMessage(`Exported ${path}`);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  const mediaUrl = selected ? api.mediaUrl(selected) : null;
  const posterUrl = selected ? api.thumbnailUrl(selected) : null;
  const shouldAutoplay = Boolean(
    selected && mediaUrl && (
      settings.autoplayMode === "always"
      || (settings.autoplayMode === "proxyAvailable" && selected.proxyAvailable)
    ),
  );
  const cutCount = assets.filter((asset) => asset.included).length;
  const reviewedCount = assets.filter((asset) => asset.status === "reviewed").length;
  const missingProxyCount = assets.filter((asset) => !asset.proxyAvailable).length;
  const missingThumbnailCount = assets.filter((asset) => asset.thumbnailQuality < 1 || !asset.thumbnailPath).length;

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    video.muted = isMuted;
    if (shouldAutoplay) void video.play().catch(() => {});
    else video.pause();
  }, [isMuted, mediaUrl, selectedId, settings.autoplayMode, shouldAutoplay]);

  function toggleMute() {
    setIsMuted((current) => {
      const next = !current;
      if (videoRef.current) videoRef.current.muted = next;
      return next;
    });
  }

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
        <div className="topbar-spacer" />
        <div className="session-readout">
          <span>{reviewedCount}/{assets.length} reviewed</span>
          <span>{cutCount} in cut</span>
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
              <button disabled={busy || missingThumbnailCount === 0} onClick={makeMissingThumbnails} aria-keyshortcuts="T">
                <span>{missingThumbnailCount ? `Generate ${missingThumbnailCount} thumbnails` : "Thumbnails ready"}</span>
                <kbd>T</kbd>
                <small>Quality 1 · frame at 20%</small>
              </button>
            </div>
          )}
        </div>
        <button className="button secondary" disabled={busy || cutCount === 0} onClick={exportCut} aria-keyshortcuts="X">
          Export OTIO <kbd>X</kbd>
        </button>
        <button className="button primary" disabled={busy || !projectId} onClick={importFolder} aria-keyshortcuts="M">
          Add media <kbd>M</kbd>
        </button>
        <button className="shortcut-help-button" onClick={() => setShowShortcuts(true)} aria-label="Keyboard shortcuts" aria-keyshortcuts="?">?</button>
      </header>

      <section className="status-line" aria-live="polite">
        <span className={busy ? "pulse" : "status-dot"} />
        {operationProgress ? (
          <div className="operation-progress">
            <span>{operationProgress.kind === "thumbnail" ? "Generating thumbnails…" : "Generating proxies…"}</span>
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
            <span className="progress-eta">{formatEta(operationProgress.estimatedRemainingSeconds)} · {operationProgress.workerCount} worker{operationProgress.workerCount === 1 ? "" : "s"}</span>
          </div>
        ) : <span>{message}</span>}
        {error && <span className="error-message">{error}</span>}
      </section>

      <div className="workbench">
        <aside className="queue-panel">
          <div className="panel-heading">
            <div>
              <span className="eyebrow">Review queue</span>
              <h1>{project?.name ?? "No project"}</h1>
            </div>
            <span className="count-badge">{filteredAssets.length}</span>
          </div>
          <label className="search-box">
            <span className="sr-only">Filter clips</span>
            <input
              id="queue-search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Filter filename, camera, notes…"
            />
            <kbd>/</kbd>
          </label>
          <div className="queue-list" ref={queueRef} role="listbox" aria-label="Clips" tabIndex={0}>
            {filteredAssets.length === 0 ? (
              <div className="empty-queue">
                <strong>No clips here yet.</strong>
                <span>Add a NAS or drive folder to begin.</span>
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
                  <span className={`review-pip ${asset.status}`} />
                  <span className="queue-thumbnail">
                    {thumbnailUrl ? <img src={thumbnailUrl} alt="" loading="lazy" /> : <span className="thumbnail-empty">Q0</span>}
                    <span className="queue-index">{String(index + 1).padStart(3, "0")}</span>
                  </span>
                  <span className="queue-main">
                    <strong>{asset.filename}</strong>
                    <span>{asset.camera}</span>
                  </span>
                  <span className="queue-tail">
                    <time>{formatDuration(asset.durationSeconds)}</time>
                    {asset.included && <span className="cut-flag">CUT</span>}
                  </span>
                </button>
              );
            })}
          </div>
          <div className="shortcut-strip" aria-label="Keyboard shortcuts">
            <span><kbd>J</kbd><kbd>K</kbd> move</span>
            <span><kbd>Space</kbd> play</span>
            <span><kbd>R</kbd> reviewed</span>
            <span><kbd>E</kbd> cut</span>
          </div>
        </aside>

        <section className="viewer-panel">
          <div className="viewer-stage">
            {selected ? (
              mediaUrl ? (
                <video ref={videoRef} key={mediaUrl} src={mediaUrl} poster={posterUrl ?? undefined} controls preload="metadata" autoPlay={shouldAutoplay} muted={isMuted} />
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
          </div>

          <div className="transport-bar">
            <button className="transport-button" onClick={() => moveSelection(-1)} aria-label="Previous clip" aria-keyshortcuts="ArrowUp K">←</button>
            <button
              className="transport-button play"
              onClick={() => videoRef.current?.paused ? void videoRef.current.play() : videoRef.current?.pause()}
              aria-label="Play or pause"
              aria-keyshortcuts="Space"
            >▶</button>
            <button className="transport-button" onClick={() => moveSelection(1)} aria-label="Next clip" aria-keyshortcuts="ArrowDown J">→</button>
            <div className="transport-divider" />
            <span className="timecode">{selected ? formatDuration(selected.durationSeconds) : "0:00"}</span>
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
                className={`button ${selected.included ? "active-cut" : "secondary"}`}
                onClick={() => persistAsset(selected, { included: !selected.included })}
                aria-keyshortcuts="E"
              >
                {selected.included ? "In rough cut" : "Add to rough cut"} <kbd>E</kbd>
              </button>
            )}
          </div>

          <div className="chronology">
            <div className="chronology-heading">
              <div>
                <span className="eyebrow">Story ribbon</span>
                <strong>{assets.length ? `${assets.length} clips in relative order` : "Relative order across sources"}</strong>
              </div>
              <div className="chronology-actions">
                <span className="chronology-position">
                  {selected ? `#${assets.findIndex((asset) => asset.id === selected.id) + 1} · ${formatDate(selected.bestTime)}` : "No clip selected"}
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
                      width: `${Math.max(chronologyZoom, assets.length * chronologyZoom)}px`,
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
                      {assets.map((asset, index) => (
                        <span className={asset.id === selectedId ? "active" : ""} style={{ left: index * chronologyZoom }} key={asset.id}>
                          {String(index + 1).padStart(3, "0")}
                        </span>
                      ))}
                    </div>
                    {lanes.map(([camera, cameraAssets]) => (
                      <div className="lane-track" key={camera}>
                        {cameraAssets.map((asset) => {
                          const index = assets.findIndex((candidate) => candidate.id === asset.id);
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
                              {asset.included && <i aria-hidden="true" />}
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
                <h2>{selected.filename}</h2>
                <span className={selected.available ? "availability online" : "availability offline"}>
                  {selected.available ? "Media available" : "Original offline"}
                </span>
              </div>

              <section className="inspector-section">
                <span className="section-label">Review state</span>
                <div className="segmented-control">
                  {(["unreviewed", "reviewed", "needs_attention", "skipped"] as ReviewStatus[]).map((status) => (
                    <button
                      key={status}
                      className={selected.status === status ? "active" : ""}
                      onClick={() => persistAsset(selected, { status })}
                      aria-keyshortcuts={{ unreviewed: "U", reviewed: "R", needs_attention: "A", skipped: "S" }[status]}
                    >
                      {statusLabels[status]} <kbd>{{ unreviewed: "U", reviewed: "R", needs_attention: "A", skipped: "S" }[status]}</kbd>
                    </button>
                  ))}
                </div>
              </section>

              <section className="inspector-section notes-section">
                <label htmlFor="notes" className="section-label">Notes <kbd>N</kbd></label>
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

              <section className="inspector-section metadata-grid">
                <span className="section-label full-row">Source</span>
                <div><span>Camera</span><strong>{selected.camera}</strong></div>
                <div><span>Format</span><strong>{selected.codec.toUpperCase()} · {formatBytes(selected.sizeBytes)}</strong></div>
                <div><span>Raw time</span><strong className="mono">{formatDate(selected.capturedAtRaw)}</strong></div>
                <div><span>Best time</span><strong className="mono">{formatDate(selected.bestTime)}</strong></div>
              </section>

              <section className="inspector-section">
                <span className="section-label">Rough-cut range</span>
                <div className="range-row">
                  <label>In <kbd>I</kbd>
                    <input
                      type="number"
                      min="0"
                      max={selected.sourceOut}
                      step="0.1"
                      value={selected.sourceIn}
                      onChange={(event) => replaceAsset({ ...selected, sourceIn: Number(event.target.value) })}
                      onBlur={() => persistAsset(selected, { sourceIn: selected.sourceIn })}
                    />
                  </label>
                  <label>Out <kbd>O</kbd>
                    <input
                      type="number"
                      min={selected.sourceIn}
                      max={selected.durationSeconds}
                      step="0.1"
                      value={selected.sourceOut}
                      onChange={(event) => replaceAsset({ ...selected, sourceOut: Number(event.target.value) })}
                      onBlur={() => persistAsset(selected, { sourceOut: selected.sourceOut })}
                    />
                  </label>
                </div>
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
                    <span>{location.project.assetCount} clips · {location.project.reviewedCount} reviewed</span>
                  </button>
                ))}
              </div>
            ) : (
              <p className="project-library-empty">No VlogCutter projects found in this folder.</p>
            )}
            <div className="modal-divider"><span>New project</span></div>
            <p>Originals stay where they are. The catalog, proxies, and exports will live in this project’s folder.</p>
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
              <div><kbd>R</kbd><kbd>U</kbd><kbd>S</kbd><kbd>A</kbd><span>Review state</span></div>
              <div><kbd>E</kbd><span>Toggle rough cut</span></div>
              <div><kbd>N</kbd><span>Focus notes</span></div>
              <div><kbd>Enter</kbd><span>Save note</span></div>
              <div><kbd>⇧ Enter</kbd><span>Save note and advance</span></div>
              <div><kbd>⌘ Enter</kbd><span>Insert a note line break</span></div>
              <div><kbd>Esc</kbd><span>Cancel note edit</span></div>
              <div><kbd>/</kbd><span>Search clips</span></div>
              <div><kbd>[</kbd><kbd>]</kbd><span>Zoom story ribbon</span></div>
              <div><kbd>C</kbd><span>Center selected clip in ribbon</span></div>
              <div><kbd>I</kbd><kbd>O</kbd><span>Set range at playhead</span></div>
              <div><kbd>P</kbd><span>Proxy selected clip</span></div>
              <div><kbd>⇧P</kbd><span>Generate missing proxies</span></div>
              <div><kbd>T</kbd><span>Generate missing thumbnails</span></div>
              <div><kbd>M</kbd><span>Add media</span></div>
              <div><kbd>X</kbd><span>Export OTIO</span></div>
              <div><kbd>⌘N</kbd><kbd>⌘O</kbd><span>New / open project</span></div>
              <div><kbd>⌘,</kbd><span>Open global settings</span></div>
              <div><kbd>Tab</kbd><kbd>Enter</kbd><span>Navigate / activate every control</span></div>
              <div><kbd>?</kbd><span>Open this keyboard map</span></div>
            </div>
          </section>
        </div>
      )}
    </main>
  );
}

export default App;
