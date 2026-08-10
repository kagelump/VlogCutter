import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type { AnalysisProgress, AnalysisReadiness, AnalysisRunRecord, AnalysisRunReport, AnalysisRunRequest, AnalysisStage, ArchiveRoot, Asset, AssetAnalysisIndexEntry, AssetAnalysisView, AssetPatch, AutoplayMode, BaselineImportResult, BaselinePreviewResult, CardCandidate, ClipTranscript, DesktopSettings, GenerationProgress, ImportPreflight, ImportProgress, ImportRunReport, IngestReport, LoadedCatalog, MediaOrigin, ModelInstallProgress, ModelStatus, PathAnalysis, Project, ProjectLibrary, ProjectPairingStatus, RelinkReport, RootMappingEntry, ServerConnectionStatus, ServerPairingResult, ServerVersionInfo, SyncStatus, ThumbnailReport } from "./types";

const inTauri = () => "__TAURI_INTERNALS__" in window;
const inAutomatedTest = () => typeof navigator !== "undefined" && navigator.userAgent.includes("jsdom");

const mockProject: Project = {
  id: "project_demo",
  name: "Hokkaido field test",
  createdAt: "2026-07-18T04:20:00.000Z",
  assetCount: 7,
  labeledCount: 4,
  removedCount: 1,
  originNodeId: null,
  pairedServerNodeId: null,
  pairedAt: null,
};

let mockAssets: Asset[] = [
  ["C0163.MP4", "Sony A7C II", "2026-02-08T00:14:08.000Z", 18.4],
  ["DJI_0342.MP4", "DJI Pocket 3", "2026-02-08T00:18:41.000Z", 12.8],
  ["IMG_8841.MOV", "Apple iPhone 17 Pro", "2026-02-08T00:27:19.000Z", 8.2],
  ["C0164.MP4", "Sony A7C II", "2026-02-08T00:31:04.000Z", 24.1],
  ["VID_20260208_0938.mp4", "Insta360 X5", "2026-02-08T00:38:55.000Z", 31.7],
  ["DJI_0343.MP4", "DJI Pocket 3", "2026-02-08T00:43:12.000Z", 14.6],
  ["C0165.MP4", "Sony A7C II", "2026-02-08T00:52:31.000Z", 19.9],
].map(([filename, camera, bestTime, duration], index) => ({
  id: `asset_demo_${index}`,
  projectId: mockProject.id,
  path: `/Volumes/Travel/Hokkaido/${filename}`,
  filename: String(filename),
  fingerprint: `demo${index}`,
  sizeBytes: 420_000_000 + index * 17_000_000,
  durationSeconds: Number(duration),
  frameRate: 59.94,
  width: 3840,
  height: 2160,
  codec: "hevc",
  camera: String(camera),
  capturedAtRaw: String(bestTime),
  bestTime: String(bestTime),
  offsetSeconds: index === 4 ? -3600 : 0,
  proxyPath: index < 5 ? `/tmp/rcdvlog-demo/${index}.mp4` : null,
  proxyRepresentation: index < 5 ? "metadata_baked_v1" : null,
  metadataRotationDegrees: 0,
  defaultRotationDegrees: 0,
  rotationDegrees: 0,
  orientationProbed: true,
  originalAvailable: index < 5,
  proxyAvailable: index < 5,
  thumbnailPath: null,
  thumbnailQuality: 0,
  notes: index === 0 ? "First clean view of the station clock." : index === 3 ? "Good transition into the market." : "",
  // Analysis fills these in; clips it has not reached keep an empty title and
  // fall back to the filename in the UI.
  title: index === 0 ? "Train pulls into Otaru station" : index === 2 ? "Handheld walk past shuttered stalls" : "",
  description: index === 0
    ? "A local train slows along the platform as an announcement plays; the camera pans left to follow it past the station clock."
    : index === 2
      ? "Unsteady handheld shot down a covered arcade. Heavy motion blur throughout."
      : "",
  labels: index === 0 ? ["arrival", "establishing"] : index === 1 ? ["train"] : index === 3 ? ["market", "transition"] : [],
  removed: index === 6,
  mediaObjectId: null,
  mediaVerificationState: "unlinked",
  available: index < 5,
  // Leave the final active clip untagged so the browser preview exercises the
  // ribbon's real add-context affordance instead of showing only finished runs.
  phase: index < 3 ? "Arrival" : index < 5 ? "Market morning" : null,
  location: index < 3 ? "Otaru" : index < 5 ? "Sapporo" : null,
  timezone: index < 5 ? "Asia/Tokyo" : null,
  storyBeat: index < 3 ? "Finding our bearings" : index < 5 ? "First discoveries" : null,
})) as Asset[];

const defaultDesktopSettings: DesktopSettings = {
  projectsRoot: "/Users/demo/Movies/VideoProjects",
  nodeId: "node_desktop_browser_demo",
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
};

const mockCard: CardCandidate = {
  sessionId: "card_4f2a9c7d",
  label: "DJI_SD",
  mountPath: "/Volumes/DJI_SD",
  cameraHint: "DJI Pocket 3",
  clipCount: 243,
  totalBytes: 80_000_000_000,
};

const mockArchiveRoot: ArchiveRoot = {
  id: "archive_nas_video",
  displayName: "NAS Video",
  localMountPath: "/Volumes/nas/VideoArchive",
  available: true,
  availableBytes: 1_200_000_000_000,
  createdAt: "2026-08-09T09:00:00Z",
};

function mockImportPreflight(sourcePath: string): ImportPreflight {
  return {
    sourcePath,
    card: mockCard,
    destination: {
      copies: true,
      archiveRoot: mockArchiveRoot,
      resolvedPath: "/Volumes/nas/VideoArchive/hokkaido-field-test/DJI_SD-4f2a9c7d",
      summary: "Originals are copied to “NAS Video”, and stay on the card too",
      bytesToCopy: 62_400_000_000,
      availableBytes: mockArchiveRoot.availableBytes,
      ready: true,
      blockedReason: "",
    },
    scanned: 243,
    newClips: 18,
    knownClips: 212,
    relinkable: 0,
    missingProxies: 31,
    collisions: 0,
    unreadable: 0,
    items: Array.from({ length: 8 }, (_, index) => ({
      path: `${sourcePath}/DCIM/100MEDIA/DJI_${String(index + 1).padStart(4, "0")}.MP4`,
      relativePath: `DCIM/100MEDIA/DJI_${String(index + 1).padStart(4, "0")}.MP4`,
      filename: `DJI_${String(index + 1).padStart(4, "0")}.MP4`,
      sizeBytes: 3_466_666_666,
      classification: index < 3 ? "new" : "knownVerified",
      assetId: index < 3 ? null : `asset_demo_${index}`,
      error: "",
    })),
  };
}

function readMockSettings(): DesktopSettings {
  try {
    const stored = window.localStorage.getItem("rcdvlog.desktopSettings");
    return stored ? { ...defaultDesktopSettings, ...JSON.parse(stored) } : defaultDesktopSettings;
  } catch {
    return defaultDesktopSettings;
  }
}

function writeMockSettings(settings: DesktopSettings) {
  try {
    window.localStorage.setItem("rcdvlog.desktopSettings", JSON.stringify(settings));
  } catch {
    // The browser preview can still use in-memory defaults when storage is unavailable.
  }
}

/**
 * The loopback origin review media is served from. Held in memory only: it is
 * minted per session by the desktop app and is meaningless to a later one.
 * `mediaUrl` is synchronous, so it reads this rather than awaiting a command.
 */
let mediaOrigin: MediaOrigin | null = null;

function rememberMediaOrigin(settings: DesktopSettings): DesktopSettings {
  mediaOrigin = settings.mediaOrigin ?? null;
  publishDevMediaHandle();
  return settings;
}

/**
 * The dev-only agent seam.
 *
 * An agent debugging media through the eval bridge otherwise has no way to
 * learn the origin's ephemeral port or its session token, and so cannot ask the
 * app why a clip will not play. `debugUrl` is set only by a debug build started
 * with `RCDVLOG_MEDIA_DEBUG=1`, which is what keeps this handle out of a
 * shipped app:
 *
 *     RCDVLOG_EVAL_BRIDGE=1 RCDVLOG_MEDIA_DEBUG=1 pnpm dev
 *     pnpm eval -- 'window.__RCDVLOG_MEDIA_ORIGIN__.debugUrl'
 *
 * See `docs/tasks/desktop/MEDIA_DELIVERY.md`.
 */
function publishDevMediaHandle() {
  const global = window as unknown as Record<string, unknown>;
  if (mediaOrigin?.debugUrl) global.__RCDVLOG_MEDIA_ORIGIN__ = mediaOrigin;
  else delete global.__RCDVLOG_MEDIA_ORIGIN__;
}

// ── Browser-preview mocks for clip analysis ─────────────────────────
// These exist so layout, focus, progress and cancel behavior can be worked on
// in the Vite preview. They prove nothing about Keychain access, model
// downloads, or real media processing.

let mockModels: ModelStatus[] = [
  { name: "ggml-large-v3-turbo-q5_0.bin", installed: false, bytes: 574_000_000, url: "https://huggingface.co/ggerganov/whisper.cpp", directory: "/Users/demo/Library/Application Support/com.rcd.vlogcutter/models" },
  { name: "ggml-silero-v5.1.2.bin", installed: true, bytes: 885_000, url: "https://huggingface.co/ggml-org/whisper-vad", directory: "/Users/demo/Library/Application Support/com.rcd.vlogcutter/models" },
];
let mockKeyPresent = false;
let mockAnalysisCancelled = false;
let mockAnalysisProgress: ((progress: AnalysisProgress) => void) | null = null;

const mockRuns: AnalysisRunRecord[] = [
  {
    id: "run_demo_1",
    projectId: mockProject.id,
    startedAt: "2026-08-03T11:02:00.000Z",
    finishedAt: "2026-08-03T11:12:29.000Z",
    status: "completed",
    stageReached: "analysis",
    proxiesGenerated: 0,
    clipsTranscribed: 4,
    clipsAnalyzed: 6,
    costUsd: 0.00065,
    budgetExhausted: false,
    error: "",
  },
];

const mockTranscripts: Record<string, ClipTranscript> = {
  asset_demo_0: {
    assetId: "asset_demo_0",
    recipeKey: "stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1",
    sourceFingerprint: "proxy-demo-0",
    status: "speech",
    rejectReason: "",
    primaryLanguage: "ja",
    speechSeconds: 7.4,
    windowCount: 1,
    segments: [
      { startMs: 1200, endMs: 4100, language: "ja", text: "次は小樽、小樽です。", meanTokenProbability: 0.81 },
      { startMs: 5200, endMs: 8600, language: "ja", text: "お出口は右側です。", meanTokenProbability: 0.77 },
    ],
  },
  asset_demo_1: {
    assetId: "asset_demo_1",
    recipeKey: "stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1",
    sourceFingerprint: "proxy-demo-1",
    status: "no_speech",
    rejectReason: "",
    primaryLanguage: "",
    speechSeconds: 0,
    windowCount: 0,
    segments: [],
  },
};

const mockAnalyses: Record<string, AssetAnalysisView> = {
  asset_demo_0: {
    analysis: {
      assetId: "asset_demo_0",
      recipeKey: "framepack.2fps.skip400ms.max40.win3-8.sharp-bright-motion.dhash8-d5.v1",
      promptVersion: "vlm.observe.v2",
      provider: "openrouter",
      model: "qwen/qwen3-vl-flash",
      status: "ok",
      rejectReason: "",
      usable: true,
      unusableReason: null,
      bestMomentStartMs: 4200,
      bestMomentEndMs: 9800,
      thumbnailTimestampMs: 6100,
      cameraMovement: "pan",
      ocrText: ["小樽駅", "OTARU"],
      transcriptUsed: true,
      promptTokens: 2411,
      completionTokens: 188,
      costUsd: 0.000112,
      latencyMs: 3820,
      attempts: 1,
      createdAt: "2026-08-03T11:04:12.000Z",
    },
    aiLabels: ["station", "arrival"],
    current: true,
  },
  asset_demo_2: {
    analysis: {
      assetId: "asset_demo_2",
      recipeKey: "framepack.2fps.skip400ms.max40.win3-8.sharp-bright-motion.dhash8-d5.v1",
      promptVersion: "vlm.observe.v2",
      provider: "openrouter",
      model: "qwen/qwen3-vl-flash",
      status: "ok",
      rejectReason: "",
      usable: false,
      unusableReason: "blurry",
      bestMomentStartMs: null,
      bestMomentEndMs: null,
      thumbnailTimestampMs: 1400,
      cameraMovement: "handheld",
      ocrText: [],
      transcriptUsed: false,
      promptTokens: 2180,
      completionTokens: 96,
      costUsd: 0.000098,
      latencyMs: 2910,
      attempts: 1,
      createdAt: "2026-08-03T11:05:02.000Z",
    },
    aiLabels: [],
    current: true,
  },
};

function mockReadiness(projectId: string): AnalysisReadiness {
  const active = mockAssets.filter((asset) => !asset.removed);
  const missingAnalyses = active.filter((asset) => !mockAnalyses[asset.id]).length;
  return {
    projectId,
    totalClips: active.length,
    missingProxies: active.filter((asset) => !asset.proxyAvailable).length,
    missingTranscripts: active.filter((asset) => !mockTranscripts[asset.id]).length,
    missingAnalyses,
    models: mockModels,
    modelsReady: mockModels.every((model) => model.installed),
    apiKeyPresent: mockKeyPresent,
    estimatedCostUsd: missingAnalyses * 0.0001084,
    costBasis: "observed",
    observedCostPerClipUsd: 0.0001084,
    lastRun: mockRuns[0] ?? null,
  };
}

async function mockAnalyzeProject(): Promise<AnalysisRunReport> {
  mockAnalysisCancelled = false;
  const stages: AnalysisStage[] = ["proxies", "transcripts", "analysis"];
  const active = mockAssets.filter((asset) => !asset.removed);
  let cancelled = false;
  let costUsd = 0;
  for (const [stageIndex, stage] of stages.entries()) {
    for (const [index, asset] of active.entries()) {
      if (mockAnalysisCancelled) {
        cancelled = true;
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 140));
      if (stage === "analysis") costUsd += 0.0001084;
      mockAnalysisProgress?.({
        stage,
        stageIndex,
        stageCount: stages.length,
        asset,
        estimate: {
          completedItems: index + 1,
          totalItems: active.length,
          completedWorkUnits: index + 1,
          totalWorkUnits: active.length,
          workerCount: stage === "transcripts" ? 1 : 4,
          workUnitsPerWorkerSecond: 1.4,
          elapsedSeconds: (index + 1) * 0.14,
          estimatedRemainingSeconds: (active.length - index - 1) * 0.14,
        },
        costUsd,
      });
    }
    if (cancelled) break;
  }
  return {
    runId: "run_demo_mock",
    status: cancelled ? "cancelled" : "completed",
    stageReached: cancelled ? "transcripts" : "analysis",
    proxies: { generated: 2, skipped: 4, cancelled: false, failures: [] },
    transcripts: { transcribed: 4, skipped: 2, cancelled, failures: [] },
    analysis: { analyzed: cancelled ? 0 : 6, skipped: 0, rejected: 0, fallbackServed: 0, costUsd, cancelled, budgetExhausted: false, failures: [] },
    costUsd,
  };
}

export const api = {
  async prerequisiteStatus(): Promise<Array<{ name: string; path: string; available: boolean }>> {
    if (!inTauri()) return ["ffmpeg", "ffprobe", "exiftool"].map((name) => ({ name, path: `/opt/homebrew/bin/${name}`, available: true }));
    return invoke("prerequisite_status");
  },

  async listProjects(): Promise<Project[]> {
    return inTauri() ? invoke("list_projects") : [mockProject];
  },

  async chooseCatalog(): Promise<string | null> {
    if (!inTauri()) return null;
    const path = await open({
      multiple: false,
      title: "Open an RCD VlogCutter catalog",
      filters: [{ name: "SQLite catalog", extensions: ["sqlite3", "sqlite", "db"] }],
    });
    return typeof path === "string" ? path : null;
  },

  async loadCatalog(path: string): Promise<LoadedCatalog> {
    if (!inTauri()) return { path, projects: [mockProject] };
    return invoke("load_catalog", { path });
  },

  async projectLibrary(): Promise<ProjectLibrary> {
    if (!inTauri()) {
      const folderPath = "/Users/demo/Movies/VideoProjects/Hokkaido field test";
      return {
        rootPath: "/Users/demo/Movies/VideoProjects",
        projects: [{ folderPath, catalogPath: `${folderPath}/catalog.sqlite3`, project: mockProject }],
      };
    }
    return invoke("project_library");
  },

  async chooseProjectsRoot(): Promise<string | null> {
    if (!inTauri()) return null;
    const path = await open({
      directory: true,
      multiple: false,
      title: "Choose the default VlogCutter projects folder",
    });
    return typeof path === "string" ? path : null;
  },

  async setProjectsRoot(path: string): Promise<ProjectLibrary> {
    if (!inTauri()) {
      writeMockSettings({ ...readMockSettings(), projectsRoot: path });
      return { rootPath: path, projects: [] };
    }
    return invoke("set_projects_root", { path });
  },

  async desktopSettings(): Promise<DesktopSettings> {
    if (!inTauri()) return readMockSettings();
    return rememberMediaOrigin(await invoke<DesktopSettings>("desktop_settings"));
  },

  async setAutoplayMode(autoplayMode: AutoplayMode): Promise<DesktopSettings> {
    if (inTauri()) {
      return rememberMediaOrigin(await invoke<DesktopSettings>("set_autoplay_mode", { autoplayMode }));
    }
    const settings = { ...readMockSettings(), autoplayMode };
    writeMockSettings(settings);
    return settings;
  },

  /**
   * Re-check the loopback media origin after a media element failed for a
   * reason it could not name, and restart it once if it has died. A healthy
   * origin comes back unchanged, so an ordinary decode failure does not
   * invalidate every URL on screen.
   */
  async recoverMediaOrigin(): Promise<MediaOrigin | null> {
    if (!inTauri()) return null;
    const origin = await invoke<MediaOrigin | null>("recover_media_origin");
    mediaOrigin = origin ?? null;
    publishDevMediaHandle();
    return mediaOrigin;
  },

  async recordDesktopContext(projectId: string, assetId: string | null): Promise<DesktopSettings> {
    if (inTauri()) {
      return rememberMediaOrigin(await invoke<DesktopSettings>("record_desktop_context", { projectId, assetId }));
    }
    const settings = {
      ...readMockSettings(),
      lastCatalogPath: "/Users/demo/Movies/VideoProjects/Hokkaido field test/catalog.sqlite3",
      lastProjectId: projectId,
      lastAssetId: assetId,
      lastContextUpdatedAt: new Date().toISOString(),
    };
    writeMockSettings(settings);
    return settings;
  },

  async createProject(name: string): Promise<Project> {
    if (!inTauri()) return { ...mockProject, name };
    return invoke("create_project", { name });
  },

  async refreshAssetOrientation(assetId: string): Promise<Asset> {
    if (!inTauri()) {
      const asset = mockAssets.find((item) => item.id === assetId);
      if (!asset) throw new Error(`asset not found: ${assetId}`);
      return { ...asset, orientationProbed: true };
    }
    return invoke("refresh_asset_orientation", { assetId });
  },

  async chooseFolder(): Promise<string | null> {
    if (!inTauri()) return inAutomatedTest() ? null : mockCard.mountPath;
    const path = await open({ directory: true, multiple: false, title: "Choose a media folder" });
    return typeof path === "string" ? path : null;
  },

  async chooseArchiveFolder(title = "Choose an archive destination"): Promise<string | null> {
    if (!inTauri()) return null;
    const path = await open({ directory: true, multiple: false, title });
    return typeof path === "string" ? path : null;
  },

  async inspectImportSource(path: string): Promise<CardCandidate | null> {
    if (!inTauri()) return path === mockCard.mountPath ? mockCard : null;
    return invoke("inspect_import_source", { path });
  },

  async detectMountedCards(): Promise<CardCandidate[]> {
    if (!inTauri()) return inAutomatedTest() ? [] : [mockCard];
    return invoke("detect_mounted_cards");
  },

  async listArchiveRoots(): Promise<ArchiveRoot[]> {
    if (!inTauri()) return [mockArchiveRoot];
    return invoke("list_archive_roots");
  },

  async createArchiveRoot(displayName: string, path: string): Promise<ArchiveRoot> {
    if (!inTauri()) {
      return { id: "archive_mock", displayName, localMountPath: path, available: true, availableBytes: 1_000_000_000_000, createdAt: new Date().toISOString() };
    }
    return invoke("create_archive_root", { displayName, path });
  },

  async bindArchiveRoot(archiveRootId: string, path: string): Promise<ArchiveRoot> {
    if (!inTauri()) return { ...mockArchiveRoot, id: archiveRootId, localMountPath: path, available: true };
    return invoke("bind_archive_root", { archiveRootId, path });
  },

  async setImportPreferences(makeProxies: boolean, archiveRootId: string | null, cardSessionId: string | null): Promise<DesktopSettings> {
    if (!inTauri()) {
      const current = readMockSettings();
      const importArchiveRootIdsByCardSession = { ...current.importArchiveRootIdsByCardSession };
      if (cardSessionId && archiveRootId) importArchiveRootIdsByCardSession[cardSessionId] = archiveRootId;
      const updated = { ...current, makeProxiesOnImport: makeProxies, lastImportArchiveRootId: archiveRootId, importArchiveRootIdsByCardSession };
      writeMockSettings(updated);
      return updated;
    }
    return rememberMediaOrigin(await invoke<DesktopSettings>("set_import_preferences", { makeProxies, archiveRootId, cardSessionId }));
  },

  async preflightImport(projectId: string, sourcePath: string, archiveRootId: string | null): Promise<ImportPreflight> {
    if (!inTauri()) return mockImportPreflight(sourcePath);
    return invoke("preflight_import", { projectId, sourcePath, archiveRootId });
  },

  async runSmartImport(projectId: string, plan: ImportPreflight, verifyCopies: boolean, makeProxies: boolean): Promise<ImportRunReport> {
    if (!inTauri()) return {
      outcome: { imported: plan.newClips, relinked: plan.relinkable, skipped: plan.knownClips + plan.missingProxies - plan.newClips, copied: plan.newClips, bytesCopied: plan.destination.bytesToCopy, cancelled: false, failures: [] },
      proxies: makeProxies ? { generated: plan.missingProxies, skipped: plan.knownClips, cancelled: false, failures: [] } : null,
      files: plan.items.map((item) => ({ path: item.path, timestamp: null, durationSeconds: null })),
    };
    return invoke("run_smart_import", { projectId, plan, verifyCopies, makeProxies });
  },

  async cancelImport(): Promise<void> {
    if (inTauri()) return invoke("cancel_import");
  },

  async onImportProgress(callback: (progress: ImportProgress) => void): Promise<() => void> {
    if (!inTauri()) return () => {};
    return listen<ImportProgress>("import-progress", (event) => callback(event.payload));
  },

  async ingestFolder(projectId: string, path: string): Promise<IngestReport> {
    return invoke("ingest_folder", { projectId, path });
  },

  /** Asks where a volume's originals went now. Prompts for a folder; null if dismissed. */
  async chooseRelinkFolder(volumeHint: string): Promise<string | null> {
    if (!inTauri()) return null;
    const path = await open({
      directory: true,
      multiple: false,
      title: `Where is the footage from ${volumeHint} now?`,
    });
    return typeof path === "string" ? path : null;
  },

  async relinkFolder(projectId: string, path: string): Promise<RelinkReport> {
    return invoke("relink_folder", { projectId, path });
  },

  async listAssets(projectId: string): Promise<Asset[]> {
    return inTauri() ? invoke("list_assets", { projectId }) : mockAssets;
  },

  async updateAsset(assetId: string, patch: AssetPatch): Promise<Asset> {
    if (inTauri()) return invoke("update_asset", { assetId, patch });
    const current = mockAssets.find((asset) => asset.id === assetId);
    if (!current) throw new Error("Asset not found");
    const updated = { ...current, ...patch };
    mockAssets = mockAssets.map((asset) => (asset.id === assetId ? updated : asset));
    return updated;
  },

  async createNoteRevision(assetId: string, text: string): Promise<Asset> {
    if (inTauri()) return invoke("create_note_revision", { assetId, text });
    const current = mockAssets.find((asset) => asset.id === assetId);
    if (!current) throw new Error("Asset not found");
    return this.updateAsset(assetId, { notes: text, labels: current.labels, removed: current.removed, rotationDegrees: current.rotationDegrees });
  },

  async addLabel(assetId: string, label: string): Promise<Asset> {
    if (inTauri()) return invoke("add_label", { assetId, label });
    const current = mockAssets.find((asset) => asset.id === assetId);
    if (!current) throw new Error("Asset not found");
    return this.updateAsset(assetId, { notes: current.notes, labels: [...current.labels, label], removed: current.removed, rotationDegrees: current.rotationDegrees });
  },

  async removeLabel(assetId: string, label: string): Promise<Asset> {
    if (inTauri()) return invoke("remove_label", { assetId, label });
    const current = mockAssets.find((asset) => asset.id === assetId);
    if (!current) throw new Error("Asset not found");
    return this.updateAsset(assetId, { notes: current.notes, labels: current.labels.filter((value) => value.toLowerCase() !== label.trim().toLowerCase()), removed: current.removed, rotationDegrees: current.rotationDegrees });
  },

  async setRemoved(assetId: string, removed: boolean): Promise<Asset> {
    if (inTauri()) return invoke("set_removed", { assetId, removed });
    const current = mockAssets.find((asset) => asset.id === assetId);
    if (!current) throw new Error("Asset not found");
    return this.updateAsset(assetId, { notes: current.notes, labels: current.labels, removed, rotationDegrees: current.rotationDegrees });
  },

  async setRotation(assetId: string, rotationDegrees: number): Promise<Asset> {
    if (inTauri()) return invoke("set_rotation", { assetId, rotationDegrees });
    const current = mockAssets.find((asset) => asset.id === assetId);
    if (!current) throw new Error("Asset not found");
    return this.updateAsset(assetId, { notes: current.notes, labels: current.labels, removed: current.removed, rotationDegrees });
  },

  async applyClockEvent(
    projectId: string,
    camera: string,
    startFilename: string,
    offsetSeconds: number,
    label: string,
  ): Promise<void> {
    if (inTauri()) {
      await invoke("apply_clock_event", { projectId, camera, startFilename, offsetSeconds, label });
      return;
    }
    mockAssets = mockAssets.map((asset) =>
      asset.camera === camera && asset.filename >= startFilename
        ? { ...asset, offsetSeconds }
        : asset,
    );
  },

  async generateProxy(assetId: string): Promise<Asset> {
    return invoke("generate_proxy", { assetId });
  },

  async generateMissingProxies(projectId: string): Promise<{ generated: number; skipped: number; failures: Array<{ path: string; error: string }> }> {
    if (!inTauri()) return { generated: 2, skipped: 5, failures: [] };
    return invoke("generate_missing_proxies", { projectId });
  },

  async generateThumbnail(assetId: string, quality = 1): Promise<Asset> {
    if (!inTauri()) {
      const current = mockAssets.find((asset) => asset.id === assetId);
      if (!current) throw new Error("Asset not found");
      const updated = { ...current, thumbnailPath: `/tmp/rcdvlog-demo/thumbnails/${assetId}/q${quality}.jpg`, thumbnailQuality: quality };
      mockAssets = mockAssets.map((asset) => asset.id === assetId ? updated : asset);
      return updated;
    }
    return invoke("generate_thumbnail", { assetId, quality });
  },

  async generateMissingThumbnails(projectId: string, quality = 1): Promise<ThumbnailReport> {
    if (!inTauri()) return { quality, generated: mockAssets.length, skipped: 0, failures: [] };
    return invoke("generate_missing_thumbnails", { projectId, quality });
  },

  async onProxyProgress(callback: (progress: GenerationProgress) => void): Promise<() => void> {
    if (!inTauri()) return () => {};
    return listen<GenerationProgress>("proxy-progress", (event) => callback(event.payload));
  },

  async onThumbnailProgress(callback: (progress: GenerationProgress) => void): Promise<() => void> {
    if (!inTauri()) return () => {};
    return listen<GenerationProgress>("thumbnail-progress", (event) => callback(event.payload));
  },

  // ── Clip analysis ───────────────────────────────────────────────

  async analysisReadiness(projectId: string): Promise<AnalysisReadiness> {
    if (!inTauri()) return mockReadiness(projectId);
    return invoke("analysis_readiness", { projectId });
  },

  async analyzeProject(projectId: string, request: AnalysisRunRequest = {}): Promise<AnalysisRunReport> {
    if (!inTauri()) return mockAnalyzeProject();
    return invoke("analyze_project", { projectId, request });
  },

  async cancelAnalysis(): Promise<void> {
    if (!inTauri()) {
      mockAnalysisCancelled = true;
      return;
    }
    return invoke("cancel_analysis");
  },

  async assetTranscript(assetId: string): Promise<ClipTranscript | null> {
    if (!inTauri()) return mockTranscripts[assetId] ?? null;
    return invoke("asset_transcript", { assetId });
  },

  async assetAnalysis(assetId: string): Promise<AssetAnalysisView | null> {
    if (!inTauri()) return mockAnalyses[assetId] ?? null;
    return invoke("asset_analysis", { assetId });
  },

  async setClipSummary(assetId: string, title: string, description: string): Promise<Asset> {
    if (!inTauri()) {
      const current = mockAssets.find((asset) => asset.id === assetId);
      if (!current) throw new Error("Asset not found");
      const updated = { ...current, title: title.trim(), description: description.trim() };
      mockAssets = mockAssets.map((asset) => asset.id === assetId ? updated : asset);
      return updated;
    }
    return invoke("set_clip_summary", { assetId, title, description });
  },

  /// Renames a camera across a whole project. The camera on an asset is a guess
  /// from the filename and EXIF, so the ribbon's lane gutter lets you correct
  /// it. Returns the number of clips that moved lanes.
  async renameCamera(projectId: string, from: string, to: string): Promise<number> {
    const next = to.trim();
    if (!next) throw new Error("A camera name cannot be empty");
    if (!inTauri()) {
      let changed = 0;
      mockAssets = mockAssets.map((asset) => {
        if (asset.projectId !== projectId || asset.camera !== from) return asset;
        changed += 1;
        return { ...asset, camera: next };
      });
      return changed;
    }
    return invoke("rename_camera", { projectId, from, to: next });
  },

  async projectAnalysisIndex(projectId: string): Promise<AssetAnalysisIndexEntry[]> {
    if (!inTauri()) {
      return mockAssets.map((asset) => ({
        assetId: asset.id,
        transcriptStatus: mockTranscripts[asset.id]?.status ?? null,
        transcriptText: (mockTranscripts[asset.id]?.segments ?? []).map((segment) => segment.text).join(" "),
        analyzed: Boolean(mockAnalyses[asset.id]),
        usable: mockAnalyses[asset.id]?.analysis.usable ?? true,
        unusableReason: mockAnalyses[asset.id]?.analysis.unusableReason ?? "",
      }));
    }
    return invoke("project_analysis_index", { projectId });
  },

  async analysisRuns(projectId: string, limit = 5): Promise<AnalysisRunRecord[]> {
    if (!inTauri()) return mockRuns;
    return invoke("analysis_runs", { projectId, limit });
  },

  async modelStatus(): Promise<ModelStatus[]> {
    if (!inTauri()) return mockModels;
    return invoke("model_status");
  },

  async installAnalysisModels(): Promise<ModelStatus[]> {
    if (!inTauri()) {
      mockModels = mockModels.map((model) => ({ ...model, installed: true }));
      return mockModels;
    }
    return invoke("install_analysis_models");
  },

  async setOpenRouterKey(key: string): Promise<boolean> {
    if (!inTauri()) {
      mockKeyPresent = key.trim().length > 0;
      return mockKeyPresent;
    }
    return invoke("set_openrouter_key", { key });
  },

  async openRouterKeyPresent(): Promise<boolean> {
    if (!inTauri()) return mockKeyPresent;
    return invoke("openrouter_key_present");
  },

  async onAnalysisProgress(callback: (progress: AnalysisProgress) => void): Promise<() => void> {
    if (!inTauri()) {
      mockAnalysisProgress = callback;
      return () => {
        mockAnalysisProgress = null;
      };
    }
    return listen<AnalysisProgress>("analysis-progress", (event) => callback(event.payload));
  },

  async onModelInstallProgress(callback: (progress: ModelInstallProgress) => void): Promise<() => void> {
    if (!inTauri()) return () => {};
    return listen<ModelInstallProgress>("model-install-progress", (event) => callback(event.payload));
  },

  // ── Server pairing ──────────────────────────────────────────────

  async pairWithServer(endpoint: string, pairingCode: string, label: string): Promise<ServerPairingResult> {
    if (!inTauri()) {
      const mockResult: ServerPairingResult = {
        nodeId: "node_server_browser_mock",
        endpoint,
        label,
        pairedAt: new Date().toISOString(),
      };
      writeMockSettings({
        ...readMockSettings(),
        pairedServerNodeId: mockResult.nodeId,
        pairedServerEndpoint: mockResult.endpoint,
        pairedServerLabel: mockResult.label,
        pairedAt: mockResult.pairedAt,
        pairedServerEndpoints: [{ url: endpoint, kind: "lan", label }],
      });
      return mockResult;
    }
    return invoke("pair_with_server", { endpoint, pairingCode, label });
  },

  async serverConnectionStatus(): Promise<ServerConnectionStatus> {
    if (!inTauri()) {
      const settings = readMockSettings();
      return {
        paired: settings.pairedServerNodeId != null,
        nodeId: settings.pairedServerNodeId ?? null,
        endpoint: settings.pairedServerEndpoint ?? null,
        label: settings.pairedServerLabel ?? null,
        pairedAt: settings.pairedAt ?? null,
        endpoints: settings.pairedServerEndpoints ?? null,
      };
    }
    return invoke("server_connection_status");
  },

  async unpairServer(): Promise<void> {
    if (inTauri()) return invoke("unpair_server");
    writeMockSettings({
      ...readMockSettings(),
      pairedServerNodeId: null,
      pairedServerEndpoint: null,
      pairedServerLabel: null,
      pairedAt: null,
      pairedServerEndpoints: null,
    });
  },

  async fetchServerInfo(endpoint: string): Promise<ServerVersionInfo> {
    if (!inTauri()) {
      return {
        nodeId: "node_server_browser_mock",
        applicationVersion: "0.1.0",
        protocolMajor: 1,
        protocolMinor: 0,
      };
    }
    return invoke("fetch_server_info", { endpoint });
  },

  // ── Root mapping ─────────────────────────────────────────────────

  async analyzeProjectPaths(projectId: string): Promise<PathAnalysis> {
    if (!inTauri()) {
      return {
        commonPrefixes: [
          { prefix: "/Volumes/Travel/Hokkaido", assetCount: 5, sampleAssets: ["C0163.MP4", "DJI_0342.MP4", "IMG_8841.MOV"] },
          { prefix: "/Volumes/Travel/Sapporo", assetCount: 2, sampleAssets: ["C0165.MP4"] },
        ],
        totalAssets: 7,
        absolutePathCount: 7,
      };
    }
    return invoke("analyze_project_paths", { projectId });
  },

  async proposeRootMappings(pathAnalysis: PathAnalysis): Promise<RootMappingEntry[]> {
    if (!inTauri()) {
      return pathAnalysis.commonPrefixes.map((p) => ({
        localPrefix: p.prefix,
        storageRootId: `root_${p.prefix.replace(/[^a-zA-Z0-9]/g, "-").toLowerCase().slice(0, 48)}`,
        kind: "new_logical_root",
        label: `${p.prefix} (${p.assetCount} assets)`,
      }));
    }
    return invoke("propose_root_mappings", { pathAnalysis });
  },

  // ── Baseline ─────────────────────────────────────────────────────

  async previewProjectBaseline(projectId: string): Promise<BaselinePreviewResult> {
    if (!inTauri()) {
      return {
        digest: "mock-digest-001",
        preview: {
          projectId,
          projectName: "Hokkaido field test",
          assetCount: 7,
          operationCount: 3,
          conflictCount: 0,
          linkedMediaCount: 0,
          proposedLibraryMatches: 2,
          rootMappingCount: 2,
          representationManifestCount: 0,
        },
        endpointUsed: "https://rcdvlog.local",
        serverNodeId: "node_server_browser_mock",
      };
    }
    return invoke("preview_project_baseline", { projectId });
  },

  async confirmBaselineImport(projectId: string, expectedDigest: string): Promise<BaselineImportResult> {
    if (!inTauri()) {
      return {
        projectId,
        baselineDigest: expectedDigest,
        reusedExisting: false,
        state: "registered",
      };
    }
    return invoke("confirm_baseline_import", { projectId, expectedDigest });
  },

  async abandonBaselineAttempt(): Promise<void> {
    if (inTauri()) return invoke("abandon_baseline_attempt");
  },

  async projectPairingStatus(projectId: string): Promise<ProjectPairingStatus> {
    if (!inTauri()) {
      const settings = readMockSettings();
      return {
        paired: settings.pairedServerNodeId != null,
        serverNodeId: settings.pairedServerNodeId ?? null,
        pairedAt: settings.pairedAt ?? null,
      };
    }
    return invoke("project_pairing_status", { projectId });
  },

  // ── Sync ───────────────────────────────────────────────────────

  async syncStatus(projectId: string): Promise<SyncStatus> {
    if (!inTauri()) {
      return {
        projectId,
        state: "unpaired",
        lastSyncAt: null,
        lastError: null,
        localPendingOperations: 0,
        remotePendingOperations: 0,
        conflictCount: 0,
        conflictDetails: [],
        activeEndpoint: null,
        backoffUntil: null,
      };
    }
    return invoke("sync_status", { projectId });
  },

  async syncAllStatus(): Promise<SyncStatus[]> {
    if (!inTauri()) return [];
    return invoke("sync_all_status");
  },

  async syncNow(projectId: string): Promise<void> {
    if (inTauri()) return invoke("sync_now", { projectId });
  },

  async onSyncStatusChanged(callback: (status: SyncStatus) => void): Promise<() => void> {
    if (!inTauri()) return () => {};
    return listen<SyncStatus>("sync-status-changed", (event) => callback(event.payload));
  },

  /**
   * The URL the stage plays a clip from.
   *
   * Review media is served from the app's own loopback HTTP origin rather than
   * the asset protocol, because `WKWebView` will not produce video frames for a
   * custom scheme: an un-played clip has no first frame to paint, and neither
   * would a hover scrub, a filmstrip, or a contact sheet. See
   * `docs/tasks/desktop/MEDIA_DELIVERY.md`.
   *
   * The proxy-versus-original choice is unchanged and stays here, on the side
   * that also decides rotation and poster agreement; it rides along as `rep` so
   * the origin serves exactly the representation this asset was measured
   * against. The origin resolves the id through the catalog — no path is ever
   * sent — and refuses the request outright without the session token.
   */
  mediaUrl(asset: Asset): string | null {
    if (!inTauri()) return null;
    const representation = asset.proxyAvailable && asset.proxyPath
      ? "proxy"
      : asset.originalAvailable
        ? "original"
        : null;
    if (!representation || !mediaOrigin) return null;
    const query = new URLSearchParams({ rep: representation, t: mediaOrigin.token });
    return `${mediaOrigin.baseUrl}/media/${encodeURIComponent(asset.id)}?${query}`;
  },

  thumbnailUrl(asset: Asset): string | null {
    if (!inTauri() || !asset.thumbnailPath) return null;
    return convertFileSrc(asset.thumbnailPath);
  },

  isDesktop: inTauri,
};
