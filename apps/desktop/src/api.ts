import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type { AnalysisProgress, AnalysisReadiness, AnalysisRunRecord, AnalysisRunReport, AnalysisRunRequest, AnalysisStage, ArchiveRoot, Asset, AssetAnalysisIndexEntry, AssetAnalysisView, AssetPatch, AutoplayMode, BaselineImportResult, BaselinePreviewResult, CameraMapping, CardCandidate, ClipTranscript, DesktopSettings, GenerationProgress, ImportHistoryEntry, ImportPreflight, ImportPreviewItem, ImportPreviewRequest, ImportProgress, ImportRollbackPreview, ImportRunReport, ImportSessionDetail, LoadedCatalog, MediaOrigin, ModelInstallProgress, ModelStatus, PathAnalysis, Project, ProjectLibrary, ProjectPairingStatus, RelinkReport, RootMappingEntry, ServerConnectionStatus, ServerPairingResult, ServerVersionInfo, SyncStatus, ThumbnailReport } from "./types";

/**
 * Whether a backend is answering behind `@tauri-apps/api`.
 *
 * True in the desktop app, and true in the Vite preview too: `main.tsx`
 * installs the shared fake backend (`src/preview/`) before it mounts, which is
 * the same seam the CUJ harness uses. So every adapter below has exactly one
 * path, and the preview exercises the command names, argument shapes and event
 * subscriptions the desktop does — there is no demo branch left to drift.
 */
const inTauri = () => "__TAURI_INTERNALS__" in window;

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

export const api = {
  async prerequisiteStatus(): Promise<Array<{ name: string; path: string; available: boolean }>> {
    return invoke("prerequisite_status");
  },

  async listProjects(): Promise<Project[]> {
    return invoke("list_projects");
  },

  async chooseCatalog(): Promise<string | null> {
    const path = await open({
      multiple: false,
      title: "Open an RCD VlogCutter catalog",
      filters: [{ name: "SQLite catalog", extensions: ["sqlite3", "sqlite", "db"] }],
    });
    return typeof path === "string" ? path : null;
  },

  async loadCatalog(path: string): Promise<LoadedCatalog> {
    return invoke("load_catalog", { path });
  },

  async projectLibrary(): Promise<ProjectLibrary> {
    return invoke("project_library");
  },

  async chooseProjectsRoot(): Promise<string | null> {
    const path = await open({
      directory: true,
      multiple: false,
      title: "Choose the default VlogCutter projects folder",
    });
    return typeof path === "string" ? path : null;
  },

  async setProjectsRoot(path: string): Promise<ProjectLibrary> {
    return invoke("set_projects_root", { path });
  },

  async desktopSettings(): Promise<DesktopSettings> {
    return rememberMediaOrigin(await invoke<DesktopSettings>("desktop_settings"));
  },

  async setAutoplayMode(autoplayMode: AutoplayMode): Promise<DesktopSettings> {
    return rememberMediaOrigin(await invoke<DesktopSettings>("set_autoplay_mode", { autoplayMode }));
  },

  /**
   * Re-check the loopback media origin after a media element failed for a
   * reason it could not name, and restart it once if it has died. A healthy
   * origin comes back unchanged, so an ordinary decode failure does not
   * invalidate every URL on screen.
   */
  async recoverMediaOrigin(): Promise<MediaOrigin | null> {
    const origin = await invoke<MediaOrigin | null>("recover_media_origin");
    mediaOrigin = origin ?? null;
    publishDevMediaHandle();
    return mediaOrigin;
  },

  async recordDesktopContext(projectId: string, assetId: string | null): Promise<DesktopSettings> {
    return rememberMediaOrigin(await invoke<DesktopSettings>("record_desktop_context", { projectId, assetId }));
  },

  async createProject(name: string): Promise<Project> {
    return invoke("create_project", { name });
  },

  async refreshAssetOrientation(assetId: string): Promise<Asset> {
    return invoke("refresh_asset_orientation", { assetId });
  },

  async chooseFolder(): Promise<string | null> {
    const path = await open({ directory: true, multiple: false, title: "Choose a media folder" });
    return typeof path === "string" ? path : null;
  },

  async chooseArchiveFolder(title = "Choose an archive destination"): Promise<string | null> {
    const path = await open({ directory: true, multiple: false, title });
    return typeof path === "string" ? path : null;
  },

  async inspectImportSource(path: string): Promise<CardCandidate | null> {
    return invoke("inspect_import_source", { path });
  },

  async detectMountedCards(): Promise<CardCandidate[]> {
    return invoke("detect_mounted_cards");
  },

  async listArchiveRoots(): Promise<ArchiveRoot[]> {
    return invoke("list_archive_roots");
  },

  async createArchiveRoot(displayName: string, path: string): Promise<ArchiveRoot> {
    return invoke("create_archive_root", { displayName, path });
  },

  async bindArchiveRoot(archiveRootId: string, path: string): Promise<ArchiveRoot> {
    return invoke("bind_archive_root", { archiveRootId, path });
  },

  /** The camera identities this project already has footage under (#71). */
  async knownProjectCameras(projectId: string): Promise<string[]> {
    return invoke("known_project_cameras", { projectId });
  },

  /** Every stored evidence-to-camera mapping, for inspecting/managing them. */
  async listCameraMappings(projectId: string): Promise<CameraMapping[]> {
    return invoke("list_camera_mappings", { projectId });
  },

  async renameCameraMapping(projectId: string, mappingId: string, camera: string): Promise<CameraMapping> {
    return invoke("rename_camera_mapping", { projectId, mappingId, camera });
  },

  async clearCameraMapping(projectId: string, mappingId: string): Promise<void> {
    return invoke("clear_camera_mapping", { projectId, mappingId });
  },

  async setImportPreferences(makeProxies: boolean, archiveRootId: string | null, cardSessionId: string | null): Promise<DesktopSettings> {
    return rememberMediaOrigin(
      await invoke<DesktopSettings>("set_import_preferences", { makeProxies, archiveRootId, cardSessionId }),
    );
  },

  async preflightImport(
    projectId: string,
    sourcePath: string,
    archiveRootId: string | null,
    cameraOverride: string | null = null,
  ): Promise<ImportPreflight> {
    return invoke("preflight_import", { projectId, sourcePath, archiveRootId, cameraOverride });
  },

  async runSmartImport(projectId: string, plan: ImportPreflight, verifyCopies: boolean, makeProxies: boolean): Promise<ImportRunReport> {
    return invoke("run_smart_import", { projectId, plan, verifyCopies, makeProxies });
  },

  /**
   * Posters/duration/probed capture time for a bounded batch of preflight
   * rows. Callers control the bound: pass only the rows actually visible so
   * a several-hundred-clip card never decodes eagerly. A failure for one
   * file (unreadable, missing ffmpeg/ffprobe) degrades that file's `error`
   * field only; the rest of the batch still resolves.
   */
  async generateImportPreview(requests: ImportPreviewRequest[]): Promise<ImportPreviewItem[]> {
    return invoke("generate_import_preview", { requests });
  },

  async cancelImport(): Promise<void> {
    return invoke("cancel_import");
  },

  async listImportSessions(projectId: string): Promise<ImportHistoryEntry[]> {
    return inTauri() ? invoke("list_import_sessions", { projectId }) : [];
  },
  async importSession(projectId: string, sessionId: string): Promise<ImportSessionDetail> {
    return invoke("import_session", { projectId, sessionId });
  },
  async importRollbackPreview(projectId: string, sessionId: string): Promise<ImportRollbackPreview> {
    return invoke("import_rollback_preview", { projectId, sessionId });
  },
  async rollbackImportMembership(projectId: string, sessionId: string): Promise<ImportRollbackPreview> {
    return invoke("rollback_import_membership", { projectId, sessionId });
  },

  async onImportProgress(callback: (progress: ImportProgress) => void): Promise<() => void> {
    return listen<ImportProgress>("import-progress", (event) => callback(event.payload));
  },

  /** Asks where a volume's originals went now. Prompts for a folder; null if dismissed. */
  async chooseRelinkFolder(volumeHint: string): Promise<string | null> {
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
    return invoke("list_assets", { projectId });
  },

  /**
   * The one write path for a clip's judgment.
   *
   * The narrow adapters that used to sit beside this one — `create_note_revision`,
   * `add_label`, `remove_label`, `set_removed`, `set_rotation` — had no callers,
   * and their presence implied a desktop that wrote note revisions and single
   * fields when in fact every judgment has always been a whole-`AssetPatch`
   * write through here. They were removed rather than routed through: the
   * mutation lifecycle in `session/assetMutations.ts` owns this path, and one
   * write path is the property that made its failure policy stateable. The Rust
   * commands remain registered for the CLI; `src/test/cuj/contract.test.ts`
   * keeps that gap visible.
   *
   * The patch carries `expectedRevision` (#49), so a write built from a read
   * the catalog has since moved past is refused rather than applied over
   * whatever happened in between.
   */
  async updateAsset(assetId: string, patch: AssetPatch): Promise<Asset> {
    return invoke("update_asset", { assetId, patch });
  },

  /** One clip, read again — the recovery path for a write refused as stale. */
  async getAsset(assetId: string): Promise<Asset> {
    return invoke("get_asset", { assetId });
  },

  async applyClockEvent(
    projectId: string,
    camera: string,
    startFilename: string,
    offsetSeconds: number,
    label: string,
  ): Promise<void> {
    await invoke("apply_clock_event", { projectId, camera, startFilename, offsetSeconds, label });
  },

  async generateProxy(assetId: string): Promise<Asset> {
    return invoke("generate_proxy", { assetId });
  },

  async generateMissingProxies(projectId: string): Promise<{ generated: number; skipped: number; failures: Array<{ path: string; error: string }> }> {
    return invoke("generate_missing_proxies", { projectId });
  },

  async generateMissingThumbnails(projectId: string, quality = 1): Promise<ThumbnailReport> {
    return invoke("generate_missing_thumbnails", { projectId, quality });
  },

  async onProxyProgress(callback: (progress: GenerationProgress) => void): Promise<() => void> {
    return listen<GenerationProgress>("proxy-progress", (event) => callback(event.payload));
  },

  async onThumbnailProgress(callback: (progress: GenerationProgress) => void): Promise<() => void> {
    return listen<GenerationProgress>("thumbnail-progress", (event) => callback(event.payload));
  },

  // ── Clip analysis ───────────────────────────────────────────────

  async analysisReadiness(projectId: string): Promise<AnalysisReadiness> {
    return invoke("analysis_readiness", { projectId });
  },

  async analyzeProject(projectId: string, request: AnalysisRunRequest = {}): Promise<AnalysisRunReport> {
    return invoke("analyze_project", { projectId, request });
  },

  async cancelAnalysis(): Promise<void> {
    return invoke("cancel_analysis");
  },

  async assetTranscript(assetId: string): Promise<ClipTranscript | null> {
    return invoke("asset_transcript", { assetId });
  },

  async assetAnalysis(assetId: string): Promise<AssetAnalysisView | null> {
    return invoke("asset_analysis", { assetId });
  },

  async setClipSummary(assetId: string, title: string, description: string): Promise<Asset> {
    return invoke("set_clip_summary", { assetId, title, description });
  },

  /// Renames a camera across a whole project. The camera on an asset is a guess
  /// from the filename and EXIF, so the ribbon's lane gutter lets you correct
  /// it. Returns the number of clips that moved lanes.
  async renameCamera(projectId: string, from: string, to: string): Promise<number> {
    const next = to.trim();
    if (!next) throw new Error("A camera name cannot be empty");
    return invoke("rename_camera", { projectId, from, to: next });
  },

  async projectAnalysisIndex(projectId: string): Promise<AssetAnalysisIndexEntry[]> {
    return invoke("project_analysis_index", { projectId });
  },

  async analysisRuns(projectId: string, limit = 5): Promise<AnalysisRunRecord[]> {
    return invoke("analysis_runs", { projectId, limit });
  },

  async modelStatus(): Promise<ModelStatus[]> {
    return invoke("model_status");
  },

  async installAnalysisModels(): Promise<ModelStatus[]> {
    return invoke("install_analysis_models");
  },

  async setOpenRouterKey(key: string): Promise<boolean> {
    return invoke("set_openrouter_key", { key });
  },

  async openRouterKeyPresent(): Promise<boolean> {
    return invoke("openrouter_key_present");
  },

  async onAnalysisProgress(callback: (progress: AnalysisProgress) => void): Promise<() => void> {
    return listen<AnalysisProgress>("analysis-progress", (event) => callback(event.payload));
  },

  async onModelInstallProgress(callback: (progress: ModelInstallProgress) => void): Promise<() => void> {
    return listen<ModelInstallProgress>("model-install-progress", (event) => callback(event.payload));
  },

  // ── Server pairing ──────────────────────────────────────────────

  async pairWithServer(endpoint: string, pairingCode: string, label: string): Promise<ServerPairingResult> {
    return invoke("pair_with_server", { endpoint, pairingCode, label });
  },

  async serverConnectionStatus(): Promise<ServerConnectionStatus> {
    return invoke("server_connection_status");
  },

  async unpairServer(): Promise<void> {
    return invoke("unpair_server");
  },

  async fetchServerInfo(endpoint: string): Promise<ServerVersionInfo> {
    return invoke("fetch_server_info", { endpoint });
  },

  // ── Root mapping ─────────────────────────────────────────────────

  async analyzeProjectPaths(projectId: string): Promise<PathAnalysis> {
    return invoke("analyze_project_paths", { projectId });
  },

  async proposeRootMappings(pathAnalysis: PathAnalysis): Promise<RootMappingEntry[]> {
    return invoke("propose_root_mappings", { pathAnalysis });
  },

  // ── Baseline ─────────────────────────────────────────────────────

  async previewProjectBaseline(projectId: string): Promise<BaselinePreviewResult> {
    return invoke("preview_project_baseline", { projectId });
  },

  async confirmBaselineImport(projectId: string, expectedDigest: string): Promise<BaselineImportResult> {
    return invoke("confirm_baseline_import", { projectId, expectedDigest });
  },

  async abandonBaselineAttempt(): Promise<void> {
    return invoke("abandon_baseline_attempt");
  },

  async projectPairingStatus(projectId: string): Promise<ProjectPairingStatus> {
    return invoke("project_pairing_status", { projectId });
  },

  // ── Sync ───────────────────────────────────────────────────────

  async syncStatus(projectId: string): Promise<SyncStatus> {
    return invoke("sync_status", { projectId });
  },

  async syncAllStatus(): Promise<SyncStatus[]> {
    return invoke("sync_all_status");
  },

  async syncNow(projectId: string): Promise<void> {
    return invoke("sync_now", { projectId });
  },

  async onSyncStatusChanged(callback: (status: SyncStatus) => void): Promise<() => void> {
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
   *
   * Nothing is serving these bytes in the browser preview, so its settings carry
   * no origin and every clip reports no URL, which is the same state the desktop
   * is in when the origin failed to bind.
   */
  mediaUrl(asset: Asset): string | null {
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
    if (!asset.thumbnailPath) return null;
    return convertFileSrc(asset.thumbnailPath);
  },

  isDesktop: inTauri,
};
