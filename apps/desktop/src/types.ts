export interface Project {
  id: string;
  name: string;
  createdAt: string;
  assetCount: number;
  labeledCount: number;
  removedCount: number;
  originNodeId: string | null;
  pairedServerNodeId: string | null;
  pairedAt: string | null;
}

export interface LoadedCatalog {
  path: string;
  projects: Project[];
}

export interface ProjectLocation {
  catalogPath: string;
  folderPath: string;
  project: Project;
}

export interface ProjectLibrary {
  rootPath: string;
  projects: ProjectLocation[];
}

export type AutoplayMode = "never" | "proxyAvailable" | "always";

export interface ServerEndpoint {
  url: string;
  kind: string;
  label: string;
}

/**
 * The loopback HTTP origin review media is served from, and the token that
 * opens it. Minted per session by the desktop app, never persisted, and gone
 * when the process is. See `docs/tasks/desktop/MEDIA_DELIVERY.md`.
 */
export interface MediaOrigin {
  baseUrl: string;
  token: string;
  /**
   * A ready-to-fetch diagnostics URL, token included. Present only in a debug
   * build started with `RCDVLOG_MEDIA_DEBUG=1`; a release app always leaves it
   * null, which is how `api.ts` knows not to publish its dev window handle.
   */
  debugUrl?: string | null;
}

export interface DesktopSettings {
  projectsRoot: string;
  nodeId: string;
  autoplayMode: AutoplayMode;
  lastCatalogPath: string | null;
  lastProjectId: string | null;
  lastAssetId: string | null;
  lastContextUpdatedAt: string | null;
  makeProxiesOnImport: boolean;
  lastImportArchiveRootId: string | null;
  /** Stable card-session fingerprint to the archive previously chosen for it. */
  importArchiveRootIdsByCardSession: Record<string, string>;
  pairedServerNodeId?: string | null;
  pairedServerEndpoint?: string | null;
  pairedServerLabel?: string | null;
  pairedAt?: string | null;
  pairedServerEndpoints?: ServerEndpoint[] | null;
  /** Null when the origin could not bind; the stage then shows its slate. */
  mediaOrigin?: MediaOrigin | null;
}

export interface Asset {
  id: string;
  projectId: string;
  path: string;
  filename: string;
  fingerprint: string;
  sizeBytes: number;
  durationSeconds: number;
  frameRate: number;
  width: number;
  height: number;
  codec: string;
  camera: string;
  capturedAtRaw: string | null;
  bestTime: string | null;
  offsetSeconds: number;
  proxyPath: string | null;
  proxyRepresentation: "raw_v3" | "metadata_tagged_raw_v2" | "metadata_baked_v1" | null;
  metadataRotationDegrees: number;
  defaultRotationDegrees: number;
  rotationDegrees: number;
  orientationProbed: boolean;
  originalAvailable: boolean;
  proxyAvailable: boolean;
  thumbnailPath: string | null;
  thumbnailQuality: number;
  notes: string;
  title: string;
  description: string;
  labels: string[];
  removed: boolean;
  mediaObjectId: string | null;
  mediaVerificationState: "unlinked" | "provisional" | "verified" | "mismatch";
  available: boolean;
  phase?: string | null;
  location?: string | null;
  timezone?: string | null;
  storyBeat?: string | null;
}

// ── Clip analysis ────────────────────────────────────────────────────

export type AnalysisStage = "proxies" | "transcripts" | "analysis";

export type TranscriptStatus = "speech" | "no_speech" | "rejected" | "failed";

export type CameraMovement = "static" | "pan" | "tilt" | "handheld" | "tracking" | "zoom" | "other";

export type UnusableReason = "black" | "blurry" | "noise" | "overexposed" | "other";

export interface TranscriptSegment {
  startMs: number;
  endMs: number;
  language: string;
  text: string;
  meanTokenProbability: number;
}

export interface ClipTranscript {
  assetId: string;
  recipeKey: string;
  sourceFingerprint: string;
  /** `no_speech` is a success, not a failure: the VAD gate found silence. */
  status: TranscriptStatus;
  rejectReason: string;
  primaryLanguage: string;
  speechSeconds: number;
  windowCount: number;
  segments: TranscriptSegment[];
}

export interface ClipAnalysis {
  assetId: string;
  recipeKey: string;
  promptVersion: string;
  provider: string;
  model: string;
  status: "ok" | "rejected" | "failed";
  rejectReason: string;
  usable: boolean;
  unusableReason: UnusableReason | null;
  bestMomentStartMs: number | null;
  bestMomentEndMs: number | null;
  thumbnailTimestampMs: number | null;
  cameraMovement: CameraMovement;
  ocrText: string[];
  transcriptUsed: boolean;
  promptTokens: number;
  completionTokens: number;
  costUsd: number;
  latencyMs: number;
  attempts: number;
  createdAt: string;
}

export interface AssetAnalysisView {
  analysis: ClipAnalysis;
  /** Labels the analysis authored; human labels live on the asset. */
  aiLabels: string[];
  /** False when the proxy was regenerated after this observation. */
  current: boolean;
}

/** The analysis facts a queue row needs, for one clip. */
export interface AssetAnalysisIndexEntry {
  assetId: string;
  transcriptStatus: TranscriptStatus | null;
  transcriptText: string;
  analyzed: boolean;
  usable: boolean;
  unusableReason: string;
}

export interface ModelStatus {
  name: string;
  installed: boolean;
  bytes: number;
  url: string;
  directory: string;
}

export interface ModelInstallProgress {
  name: string;
  downloadedBytes: number;
  totalBytes: number;
}

export interface AnalysisRunRecord {
  id: string;
  projectId: string;
  startedAt: string;
  finishedAt: string;
  status: "running" | "completed" | "cancelled" | "failed";
  stageReached: AnalysisStage;
  proxiesGenerated: number;
  clipsTranscribed: number;
  clipsAnalyzed: number;
  costUsd: number;
  budgetExhausted: boolean;
  error: string;
}

export interface AnalysisReadiness {
  projectId: string;
  totalClips: number;
  missingProxies: number;
  missingTranscripts: number;
  missingAnalyses: number;
  models: ModelStatus[];
  modelsReady: boolean;
  apiKeyPresent: boolean;
  estimatedCostUsd: number;
  costBasis: "observed" | "reference";
  observedCostPerClipUsd: number;
  lastRun: AnalysisRunRecord | null;
}

export interface AnalysisProgress {
  stage: AnalysisStage;
  stageIndex: number;
  stageCount: number;
  asset: Asset | null;
  estimate: BatchEstimate;
  costUsd: number;
}

export interface AnalysisRunReport {
  runId: string;
  status: "completed" | "cancelled" | "failed";
  stageReached: AnalysisStage;
  proxies: { generated: number; skipped: number; cancelled: boolean; failures: Array<{ path: string; error: string }> };
  transcripts: { transcribed: number; skipped: number; cancelled: boolean; failures: Array<{ path: string; error: string }> };
  analysis: {
    analyzed: number;
    skipped: number;
    rejected: number;
    fallbackServed: number;
    costUsd: number;
    cancelled: boolean;
    budgetExhausted: boolean;
    failures: Array<{ path: string; error: string }>;
  };
  costUsd: number;
}

export interface AnalysisRunRequest {
  force?: boolean;
  budgetUsd?: number | null;
}

export interface AssetPatch {
  notes: string;
  labels: string[];
  removed: boolean;
  rotationDegrees: number;
}

export interface IngestReport {
  scanned: number;
  imported: number;
  duplicates: number;
  /** Clips already in the project whose original was offline and has now been found again here. */
  relinked: number;
  failures: Array<{ path: string; error: string }>;
}

export interface CardCandidate {
  sessionId: string;
  label: string;
  mountPath: string;
  cameraHint: string;
  clipCount: number;
  totalBytes: number;
}

export interface ArchiveRoot {
  id: string;
  displayName: string;
  localMountPath: string | null;
  available: boolean;
  availableBytes: number | null;
  createdAt: string;
}

export type ImportClassification =
  | "new"
  | "knownVerified"
  | "knownRelinkable"
  | "knownMissingProxy"
  | "pathCollision"
  | "unreadable";

export interface PreflightItem {
  path: string;
  relativePath: string;
  filename: string;
  sizeBytes: number;
  classification: ImportClassification;
  assetId: string | null;
  error: string;
}

export interface ImportDestination {
  copies: boolean;
  archiveRoot: ArchiveRoot | null;
  resolvedPath: string;
  summary: string;
  bytesToCopy: number;
  availableBytes: number | null;
  ready: boolean;
  blockedReason: string;
}

export interface ImportPreflight {
  sourcePath: string;
  card: CardCandidate | null;
  destination: ImportDestination;
  scanned: number;
  newClips: number;
  knownClips: number;
  relinkable: number;
  missingProxies: number;
  collisions: number;
  unreadable: number;
  items: PreflightItem[];
}

export type ImportStage = "scanning" | "copying" | "reading" | "proxies" | "complete";

export interface ImportProgress {
  stage: ImportStage;
  completed: number;
  total: number;
  currentFile: string;
  completedBytes: number;
  totalBytes: number;
}

export interface ImportOutcome {
  imported: number;
  relinked: number;
  skipped: number;
  copied: number;
  bytesCopied: number;
  cancelled: boolean;
  failures: Array<{ path: string; error: string }>;
}

export interface ProxyReport {
  generated: number;
  skipped: number;
  cancelled: boolean;
  failures: Array<{ path: string; error: string }>;
}

export interface ImportRunReport {
  outcome: ImportOutcome;
  proxies: ProxyReport | null;
  files: Array<{
    path: string;
    timestamp: string | null;
    durationSeconds: number | null;
  }>;
}

/** The result of pointing the app at a folder to find originals it has lost track of. */
export interface RelinkReport {
  scanned: number;
  /** Assets whose original was offline and now resolves again. */
  repaired: number;
  /** Clips found there that the project could already reach elsewhere. */
  alreadyOnline: number;
  /** Media in that folder belonging to no asset in this project. Counted, never imported. */
  unrecognized: number;
  /** Assets whose original is still not reachable anywhere after the search. */
  stillMissing: number;
  failures: Array<{ path: string; error: string }>;
}

export interface ThumbnailReport {
  quality: number;
  generated: number;
  skipped: number;
  failures: Array<{ path: string; error: string }>;
}

export interface GenerationProgress {
  asset: Asset | null;
  estimate: BatchEstimate;
}

export interface BatchEstimate {
  completedItems: number;
  totalItems: number;
  completedWorkUnits: number;
  totalWorkUnits: number;
  workerCount: number;
  workUnitsPerWorkerSecond: number | null;
  elapsedSeconds: number;
  estimatedRemainingSeconds: number | null;
}

// ── Server pairing types ─────────────────────────────────────────────

export interface ServerPairingResult {
  nodeId: string;
  endpoint: string;
  label: string;
  pairedAt: string;
}

export interface ServerConnectionStatus {
  paired: boolean;
  nodeId: string | null;
  endpoint: string | null;
  label: string | null;
  pairedAt: string | null;
  endpoints: ServerEndpoint[] | null;
}

export interface ServerVersionInfo {
  nodeId: string;
  applicationVersion: string;
  protocolMajor: number;
  protocolMinor: number;
}

// ── Root mapping types ───────────────────────────────────────────────

export interface PathPrefix {
  prefix: string;
  assetCount: number;
  sampleAssets: string[];
}

export interface PathAnalysis {
  commonPrefixes: PathPrefix[];
  totalAssets: number;
  absolutePathCount: number;
}

export interface RootMappingEntry {
  localPrefix: string;
  storageRootId: string;
  kind: string;
  label: string;
}

// ── Baseline types ───────────────────────────────────────────────────

export interface BaselinePreview {
  projectId: string;
  projectName: string;
  assetCount: number;
  operationCount: number;
  conflictCount: number;
  linkedMediaCount: number;
  proposedLibraryMatches: number;
  rootMappingCount: number;
  representationManifestCount: number;
}

export interface BaselinePreviewResult {
  digest: string;
  preview: BaselinePreview;
  endpointUsed: string;
  serverNodeId: string;
}

export interface BaselineImportResult {
  projectId: string;
  baselineDigest: string;
  reusedExisting: boolean;
  state: string;
}

export interface ProjectPairingStatus {
  paired: boolean;
  serverNodeId: string | null;
  pairedAt: string | null;
}

// ── Sync types ─────────────────────────────────────────────────────────

export type SyncState =
  | "unpaired"
  | "offline"
  | "connecting"
  | "syncing"
  | "synced"
  | "attention"
  | "incompatible"
  | "credential_revoked";

export interface OperationSummary {
  operationId: string;
  operationType: string;
  originNodeId: string;
  originSequence: number;
  camera: string | null;
  startFilename: string | null;
  offsetSeconds: number | null;
  label: string | null;
}

export interface ConflictDetail {
  conflictId: string;
  conflictType: string;
  aggregateType: string;
  aggregateId: string;
  incomingOperationSummary: OperationSummary | null;
  activeOperationSummary: OperationSummary | null;
  expectedBaseRevision: number | null;
  actualRevision: number;
  createdAt: string;
}

export interface SyncStatus {
  projectId: string;
  state: SyncState;
  lastSyncAt: string | null;
  lastError: string | null;
  localPendingOperations: number;
  remotePendingOperations: number;
  conflictCount: number;
  conflictDetails: ConflictDetail[];
  activeEndpoint: string | null;
  backoffUntil: string | null;
}
