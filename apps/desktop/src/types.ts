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

export interface DesktopSettings {
  projectsRoot: string;
  nodeId: string;
  autoplayMode: AutoplayMode;
  lastCatalogPath: string | null;
  lastProjectId: string | null;
  lastAssetId: string | null;
  lastContextUpdatedAt: string | null;
  pairedServerNodeId?: string | null;
  pairedServerEndpoint?: string | null;
  pairedServerLabel?: string | null;
  pairedAt?: string | null;
  pairedServerEndpoints?: ServerEndpoint[] | null;
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
