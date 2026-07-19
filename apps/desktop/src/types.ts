export type ReviewStatus =
  | "unreviewed"
  | "reviewed"
  | "skipped"
  | "needs_attention";

export interface Project {
  id: string;
  name: string;
  createdAt: string;
  assetCount: number;
  reviewedCount: number;
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
  autoplayMode: AutoplayMode;
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
  originalAvailable: boolean;
  proxyAvailable: boolean;
  thumbnailPath: string | null;
  thumbnailQuality: number;
  notes: string;
  status: ReviewStatus;
  included: boolean;
  sourceIn: number;
  sourceOut: number;
  available: boolean;
  phase?: string | null;
  location?: string | null;
  timezone?: string | null;
  storyBeat?: string | null;
}

export interface AssetPatch {
  notes: string;
  status: ReviewStatus;
  included: boolean;
  sourceIn: number;
  sourceOut: number;
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
