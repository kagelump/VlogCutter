export const SUPPORTED_PROTOCOL_MAJOR = 1;

export interface ProtocolVersion {
  major: number;
  minor: number;
}

export interface ServerVersion {
  requestId: string;
  applicationVersion: string;
  protocolVersion: ProtocolVersion;
  controlSchemaVersion: number;
  nodeId: string;
}

export interface ReadinessCheck {
  name: string;
  status: "ok" | "warning" | "error";
  code?: string;
}

export interface ServerReadiness {
  requestId: string;
  status: "ready" | "degraded" | "not_ready";
  checks: ReadinessCheck[];
}

export interface ServerStatus {
  version: ServerVersion;
  readiness: ServerReadiness;
}

export interface SessionInfo {
  sessionId: string;
  displayPrefix: string;
  hostname: string;
  createdAt: string;
  lastUsedAt: string;
  expiresAt: string;
  revoked: boolean;
}

export interface SessionState {
  authenticated: boolean;
  sessions: SessionInfo[];
}

export interface LoginResult {
  requestId: string;
  sessionId: string;
  displayPrefix: string;
  expiresAt: string;
}

export type GlobalMediaVerificationState = "unverified" | "provisional" | "verified" | "mismatch";
export type NodeAvailabilityState = "known" | "accessible_here" | "accessible_on_another_node" | "temporarily_unavailable" | "missing_after_completed_observation" | "fingerprint_mismatch";
export type LibrarySort = "createdDesc" | "filenameAsc" | "sizeDesc";

export interface MediaObject {
  id: string;
  sizeBytes: number | null;
  quickFingerprint: string | null;
  fullContentHash: string | null;
  verificationState: GlobalMediaVerificationState;
  createdAt: string;
  updatedAt: string;
}

export interface LibrarySearchItem {
  media: MediaObject;
  filename: string;
  cameraMake: string | null;
  cameraModel: string | null;
  primaryStorageRootId: string | null;
  storageLocationCount: number;
  projectMembershipCount: number;
  availability: NodeAvailabilityState;
  thumbnailRepresentationId: string | null;
}

export interface LibrarySearchPage {
  items: LibrarySearchItem[];
  nextCursor: string | null;
  availabilityFilterApplied: boolean;
}

export interface LibraryQueryParams {
  limit?: number;
  sort?: LibrarySort;
  verificationState?: GlobalMediaVerificationState;
  camera?: string;
  filenamePrefix?: string;
  availability?: NodeAvailabilityState;
  after?: string;
}

export interface ProjectSummary { projectId: string; state: string; assetCount: number; registeredAt: string; updatedAt: string }
export interface ProjectPage { items: ProjectSummary[]; nextCursor: string | null }
export interface ProjectAssetItem {
  projectAssetId: string; filename: string; camera: string; mediaObjectId: string | null;
  mediaVerificationState: string; removed: boolean; rotationDegrees: number; labels: string[];
  activeNote: string | null; durationSeconds: number; bestTime: string | null;
}
export interface ProjectAssetPage { items: ProjectAssetItem[]; nextCursor: string | null }

export interface ReviewRepresentation {
  representationId: string;
  state: string;
  availableOnServer: boolean;
}

export interface ProjectReviewAsset extends ProjectAssetItem {
  materializedRevision: number;
  activeNoteRevisionId: string | null;
  noteRevisionCount: number;
  proxy: ReviewRepresentation | null;
  thumbnail: ReviewRepresentation | null;
  originalAvailability: NodeAvailabilityState | null;
  unresolvedConflictCount: number;
}

export interface ReviewMutationResult {
  asset: ProjectReviewAsset;
  staleBaseRevision: boolean;
}

export interface AddToProjectRequest {
  mediaObjectIds: string[];
  acknowledgeUnverified: boolean;
  idempotencyKey: string;
}
export type AddToProjectDisposition = "ready" | "added" | "already_member" | "acknowledgement_required" | "identity_mismatch";
export interface AddToProjectItem {
  mediaObjectId: string; projectAssetId: string | null; verificationState: GlobalMediaVerificationState;
  disposition: AddToProjectDisposition; remoteOnly: boolean;
}
export interface AddToProjectPreview {
  projectId: string; selectedCount: number; readyCount: number; alreadyMemberCount: number;
  acknowledgementRequiredCount: number; mismatchCount: number; items: AddToProjectItem[];
}
export interface AddToProjectResult {
  projectId: string; idempotencyKey: string; addedCount: number; alreadyMemberCount: number;
  blockedCount: number; items: AddToProjectItem[];
}

export interface DeviceCredentialInfo { credentialId: string; tokenPrefix: string; label: string; createdAt: string; lastUsedAt: string; revoked: boolean }
export interface AuthDiagnostics { activeSessions: number; activeDeviceCredentials: number; pendingBootstrapCodes: number; nodeId: string }
export interface AdminConfigDiagnostics {
  dataDir: string; tempDir: string; derivedDir: string; backupDir: string; allowedOriginalMounts: string[];
  listenAddr: string; shutdownTimeoutSeconds: number; trustedProxyCidrCount: number; lanHostname: string;
  sessionMaxAgeSeconds: number; bootstrapCodeTtlSeconds: number; pairingCodeTtlSeconds: number;
  backupIntervalSeconds: number; backupDailyRetention: number; backupWeeklyRetention: number; minimumFreeBytes: number;
  ffmpegCommand: string; ffprobeCommand: string; exiftoolCommand: string;
}
export interface AdminOverview {
  applicationVersion: string; protocolMajor: number; protocolMinor: number; controlSchemaVersion: number;
  nodeId: string; readiness: Omit<ServerReadiness, "requestId">; config: AdminConfigDiagnostics;
  auth: AuthDiagnostics; derivedAvailableBytes: number | null; backupAvailableBytes: number | null;
}
export interface StorageRoot { id: string; displayName: string; immutableOriginal: boolean; enabled: boolean; disabledAt: string | null; createdAt: string; updatedAt: string }
export interface RootAssociationEvidence {
  matchingRelativePaths: number; matchingQuickFingerprints: number; matchingFullHashes: number; mismatches: number;
  sampledRelativePaths: string[]; capturedAt: string;
}
export interface StorageBinding {
  nodeId: string; storageRootId: string; localMountPath: string; rootSubpath: string | null;
  protocol: "nfs" | "smb" | "local" | "other"; accessMode: "read_only" | "read_write";
  verificationStatus: "pending" | "confirmed" | "unavailable" | "mismatch" | "rejected"; enabled: boolean;
  associationEvidence: RootAssociationEvidence | null;
  confirmation: { status: "pending" | "confirmed" | "rejected"; confirmedByNodeId?: string; confirmedAt?: string };
  createdAt: string; updatedAt: string;
}
export interface ScanRun {
  scanId: string; nodeId: string; storageRootId: string; scopeRelativePath: string | null; rootJobId: string;
  source: "manual" | "schedule"; scheduleId: string | null; state: string; visitedEntries: number; supportedFiles: number;
  unchangedFiles: number; queuedFiles: number; completedFiles: number; failedFiles: number; errorCode: string | null;
  createdAt: string; startedAt: string | null; completedAt: string | null;
}
export interface ScanFileDiagnostic { diagnosticId: string; scanId: string; storageObjectId: string | null; stage: string; code: string; safePathIdentity: string; retryable: boolean; recordedAt: string }
export interface ScanSchedule { scheduleId: string; storageRootId: string; scopeRelativePath: string | null; enabled: boolean; localTime: string; timezone: string; nextRunAt: string; lastEnqueuedAt: string | null; lastScanId: string | null; createdAt: string; updatedAt: string }
export type AdminJobState = "queued" | "running" | "retry_wait" | "succeeded" | "failed" | "canceled";
export interface AdminJob {
  enqueueSequence?: number; definition: { jobId: string; jobType: string; jobClass: string; targetNodeId: string; projectId: string | null; mediaObjectId: string | null; recipeKey: string; totalItems: number };
  estimate?: { completedItems: number; totalItems: number; completedWorkUnits: number; totalWorkUnits: number; workerCount: number; workUnitsPerWorkerSecond: number | null; elapsedSeconds: number; estimatedRemainingSeconds: number | null; confidence?: string; estimatedRemainingSecondsRange?: [number, number] | null };
  state: AdminJobState; attemptCount: number; retryable: boolean; availableAt: string; cancelRequestedAt: string | null; leaseActive: boolean; leaseExpiresAt: string | null; completedWorkUnits: number; totalWorkUnits: number;
  completedItems: number; failedItems: number; totalItems: number; error: { code: string; details: unknown } | null;
  outputIdentity: string | null; createdAt: string; startedAt: string | null; updatedAt: string; completedAt: string | null;
}
export interface AdminJobPage { items: AdminJob[]; nextBefore: number | null }
export interface ProjectSyncOverview { projectId: string; registryState: string; originNodeId: string | null; serverNodeId: string; assetCount: number; operationCount: number; latestLocalSequence: number; pendingProjectionRepairs: number; unresolvedConflictCount: number; nodeObservationCount: number; lastSuccessAt: string; lastErrorCode: string | null; endpoint: string | null; compatibility: string; connectivity: string; recommendations: string[] }
export interface AdminAuditEvent { eventSequence: number; occurredAt: string; actorType: string; actorId: string; action: string; targetType: string; targetId: string | null; requestId: string; detail: Record<string, unknown> }
export interface RepresentationBatchResult { selectedCount: number; reusedCount: number; queuedCount: number; succeededCount: number; failedCount: number; skippedCount: number; skipped: { mediaObjectId: string; reason: string }[] }

export type ClientErrorKind =
  | "server_unavailable"
  | "session_expired"
  | "credential_revoked"
  | "csrf_failure"
  | "protocol_mismatch"
  | "server_error";

export class ProjectClientError extends Error {
  constructor(
    public readonly kind: ClientErrorKind,
    message: string,
    public readonly code: string,
    public readonly requestId: string | null = null,
    public readonly status: number | null = null,
  ) {
    super(message);
    this.name = "ProjectClientError";
  }
}

/** Operations available to either a native or HTTP-backed project surface. */
export interface ProjectClient {
  getServerStatus(): Promise<ServerStatus>;
}

/** Browser-session operations. Native clients authenticate with device credentials instead. */
export interface BrowserProjectClient extends ProjectClient {
  getSession(): Promise<SessionState>;
  loginWithBootstrapCode(code: string, hostname?: string): Promise<LoginResult>;
  listLibraryMedia(query?: LibraryQueryParams): Promise<LibrarySearchPage>;
  listProjects(limit?: number, after?: string): Promise<ProjectPage>;
  listProjectAssets(projectId: string, limit?: number, after?: string, search?: string): Promise<ProjectAssetPage>;
  getProjectReviewAsset(projectId: string, assetId: string): Promise<ProjectReviewAsset>;
  createReviewNote(projectId: string, assetId: string, text: string, baseRevision: number): Promise<ReviewMutationResult>;
  addReviewLabel(projectId: string, assetId: string, label: string, baseRevision: number): Promise<ReviewMutationResult>;
  removeReviewLabel(projectId: string, assetId: string, label: string, baseRevision: number): Promise<ReviewMutationResult>;
  setReviewRemoved(projectId: string, assetId: string, removed: boolean, baseRevision: number): Promise<ReviewMutationResult>;
  setReviewRotation(projectId: string, assetId: string, rotationDegrees: number, baseRevision: number): Promise<ReviewMutationResult>;
  previewAddToProject(projectId: string, request: AddToProjectRequest): Promise<AddToProjectPreview>;
  addToProject(projectId: string, request: AddToProjectRequest): Promise<AddToProjectResult>;
  getAdminOverview(): Promise<AdminOverview>;
  listAdminAudit(limit?: number, before?: number): Promise<{ items: AdminAuditEvent[] }>;
  listAdminStorageRoots(): Promise<{ items: StorageRoot[] }>;
  createAdminStorageRoot(displayName: string, immutableOriginal?: boolean): Promise<StorageRoot>;
  updateAdminStorageRoot(rootId: string, update: { displayName?: string; enabled?: boolean; confirmDisable?: boolean }): Promise<StorageRoot>;
  listAdminStorageBindings(): Promise<{ items: StorageBinding[] }>;
  upsertAdminStorageBinding(rootId: string, request: { localMountPath: string; rootSubpath?: string; protocol: StorageBinding["protocol"]; accessMode: StorageBinding["accessMode"]; enabled?: boolean }): Promise<StorageBinding>;
  verifyAdminStorageBinding(rootId: string): Promise<StorageBinding>;
  confirmAdminStorageBinding(rootId: string, action: "confirm" | "reject"): Promise<StorageBinding>;
  listAdminScans(limit?: number): Promise<{ items: ScanRun[] }>;
  listAdminScanDiagnostics(scanId: string, limit?: number): Promise<{ items: ScanFileDiagnostic[] }>;
  requestAdminScan(request: { storageRootId: string; scopeRelativePath?: string; idempotencyKey: string }): Promise<ScanRun>;
  listAdminScanSchedules(): Promise<{ items: ScanSchedule[] }>;
  createAdminScanSchedule(request: { storageRootId: string; scopeRelativePath?: string; enabled: boolean; localTime: string; timezone: string }): Promise<ScanSchedule>;
  updateAdminScanSchedule(scheduleId: string, update: { enabled?: boolean; localTime?: string; timezone?: string }): Promise<ScanSchedule>;
  listAdminJobs(limit?: number, before?: number, state?: AdminJobState): Promise<AdminJobPage>;
  getAdminJob(jobId: string): Promise<AdminJob>;
  cancelAdminJob(jobId: string): Promise<AdminJob>;
  retryAdminJob(jobId: string): Promise<AdminJob>;
  listAdminSync(): Promise<{ items: ProjectSyncOverview[] }>;
  requestAdminRepresentationBatch(request: { mediaObjectIds: string[]; kind: "proxy" | "thumbnail"; idempotencyKey: string }): Promise<RepresentationBatchResult>;
  listDevices(): Promise<DeviceCredentialInfo[]>;
  revokeDevice(credentialId: string): Promise<void>;
  revokeSession(sessionId: string): Promise<void>;
}

export function assertProtocolCompatible(version: ServerVersion): void {
  if (version.protocolVersion.major !== SUPPORTED_PROTOCOL_MAJOR) {
    throw new ProjectClientError(
      "protocol_mismatch",
      `This web app supports protocol ${SUPPORTED_PROTOCOL_MAJOR}, but the server uses ${version.protocolVersion.major}.`,
      "protocol_mismatch",
      version.requestId,
    );
  }
}

export * from "./generated";
