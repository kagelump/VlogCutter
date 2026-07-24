import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type { Asset, AssetPatch, AutoplayMode, BaselineImportResult, BaselinePreviewResult, DesktopSettings, GenerationProgress, IngestReport, LoadedCatalog, PathAnalysis, Project, ProjectLibrary, ProjectPairingStatus, RootMappingEntry, ServerConnectionStatus, ServerPairingResult, ServerVersionInfo, SyncStatus, ThumbnailReport } from "./types";

const inTauri = () => "__TAURI_INTERNALS__" in window;

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
  labels: index === 0 ? ["arrival", "establishing"] : index === 1 ? ["train"] : index === 3 ? ["market", "transition"] : [],
  removed: index === 6,
  mediaObjectId: null,
  mediaVerificationState: "unlinked",
  available: index < 5,
  phase: index < 3 ? "Arrival" : "Market morning",
  location: index < 3 ? "Otaru" : "Sapporo",
  timezone: "Asia/Tokyo",
  storyBeat: index < 3 ? "Finding our bearings" : "First discoveries",
})) as Asset[];

const defaultDesktopSettings: DesktopSettings = {
  projectsRoot: "/Users/demo/Movies/VideoProjects",
  nodeId: "node_desktop_browser_demo",
  autoplayMode: "proxyAvailable",
  lastCatalogPath: null,
  lastProjectId: null,
  lastAssetId: null,
  lastContextUpdatedAt: null,
  pairedServerNodeId: null,
  pairedServerEndpoint: null,
  pairedServerLabel: null,
  pairedAt: null,
  pairedServerEndpoints: null,
};

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
    return inTauri() ? invoke("desktop_settings") : readMockSettings();
  },

  async setAutoplayMode(autoplayMode: AutoplayMode): Promise<DesktopSettings> {
    if (inTauri()) return invoke("set_autoplay_mode", { autoplayMode });
    const settings = { ...readMockSettings(), autoplayMode };
    writeMockSettings(settings);
    return settings;
  },

  async recordDesktopContext(projectId: string, assetId: string | null): Promise<DesktopSettings> {
    if (inTauri()) return invoke("record_desktop_context", { projectId, assetId });
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
    if (!inTauri()) return null;
    const path = await open({ directory: true, multiple: false, title: "Choose a media folder" });
    return typeof path === "string" ? path : null;
  },

  async ingestFolder(projectId: string, path: string): Promise<IngestReport> {
    return invoke("ingest_folder", { projectId, path });
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

  mediaUrl(asset: Asset): string | null {
    if (!inTauri()) return null;
    const path = asset.proxyAvailable && asset.proxyPath
      ? asset.proxyPath
      : asset.originalAvailable
        ? asset.path
        : null;
    return path ? convertFileSrc(path) : null;
  },

  thumbnailUrl(asset: Asset): string | null {
    if (!inTauri() || !asset.thumbnailPath) return null;
    return convertFileSrc(asset.thumbnailPath);
  },

  isDesktop: inTauri,
};
