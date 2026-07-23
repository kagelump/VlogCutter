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

export interface DesktopSettings {
  projectsRoot: string;
  nodeId: string;
  autoplayMode: AutoplayMode;
  lastCatalogPath: string | null;
  lastProjectId: string | null;
  lastAssetId: string | null;
  lastContextUpdatedAt: string | null;
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
