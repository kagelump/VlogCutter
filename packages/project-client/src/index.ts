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
// TV review targets (#221). Mirrors rcdvlog-server-core::review_targets DTOs.
// The handoff itself is opaque here: digesting and validation are Rust-owned.
export type ReviewTargetHandoffJson = Record<string, unknown>;
export type ArtifactAvailabilityState = "unknown" | "present" | "missing" | "modified";

export interface ReviewTargetImportRequest {
  handoff: ReviewTargetHandoffJson;
  declaredIssuer?: string | null;
  availability?: { state: ArtifactAvailabilityState; observedAt: string } | null;
  maxVerificationAgeSeconds?: number | null;
}

export interface ReviewTargetCheck {
  name: string;
  status: "pass" | "warn" | "fail";
  code: string | null;
  detail: string;
}

export interface ReviewTargetPreflight {
  outcome: "new" | "duplicate" | "refresh" | "conflict" | "blocked";
  importable: boolean;
  checks: ReviewTargetCheck[];
  preview: {
    projectId: string;
    revisionId: string;
    viewedArtifactId: string;
    targetDigest: string;
    movieBlake3: string;
    durationSeconds: number;
    cutCount: number;
    youtubeVideoId: string | null;
    verifiedAt: string;
  } | null;
  existingReviewTargetId: string | null;
  conflictingReviewTargetIds: string[];
  maxVerificationAgeSeconds: number;
}

export interface ReviewTargetActor { actorType: string; actorId: string }

export interface ReviewTargetReadiness {
  ready: boolean;
  freshness: "fresh" | "expired" | "future";
  verificationAgeSeconds: number;
  maxVerificationAgeSeconds: number;
  availabilityState: ArtifactAvailabilityState;
  blockers: string[];
}

export interface ReviewTargetSummary {
  reviewTargetId: string;
  projectId: string;
  revisionId: string;
  viewedArtifactId: string;
  targetDigest: string;
  durationSeconds: number;
  cutCount: number;
  youtube: { videoId: string | null; declaredBy: "export" | "serverRebind" | null; identityStatus: "declaredUnverified" | "absent" };
  importedAt: string;
  latestVerifiedAt: string;
  readiness: ReviewTargetReadiness;
}

export type DeclaredAudio =
  | null
  | { mode: "silent" }
  | { mode: "source"; version: number; assetId: string; sourceInSeconds: number; sourceOutSeconds: number; timelineStartSeconds: number; syncOffsetSeconds: number; gainDb: number; fadeInSeconds: number; fadeOutSeconds: number };

export interface ReviewTargetCut {
  operationId: string;
  position: number;
  assetId: string;
  sourceInSeconds: number;
  sourceOutSeconds: number;
  movieInPts: number;
  movieOutPts: number;
  outputFrameCount: number;
  declaredAudio: DeclaredAudio;
  movieInSeconds: number;
  movieOutSeconds: number;
}

export interface ReviewTargetDetail extends ReviewTargetSummary {
  schemaVersion: number;
  planDigest: string;
  constraintsKey: string;
  movie: {
    blake3: string;
    byteLength: number;
    durationSeconds: number;
    timeBaseNumerator: number;
    timeBaseDenominator: number;
    originPts: number;
    videoFrameCount: number;
    audioSampleRate: number;
    audioChannels: number;
  };
  mapping: { status: string; picturePrecision: string; sourceRangePrecision: string; audioDeclaration: string; cutCount: number; pictureEndSeconds: number };
  cuts: ReviewTargetCut[];
  provenance: { importedAt: string; importer: ReviewTargetActor; declaredIssuer: string | null; issuerVerification: "unverified"; derivedFromReviewTargetId: string | null };
  observations: Array<{
    observationId: string;
    verification: { verifiedAt: string; outputMatches: boolean; inputsMatch: boolean; constraintsMatch: boolean; planRenderable: boolean; fullyDecoded: boolean };
    availabilityState: ArtifactAvailabilityState;
    availabilityObservedAt: string;
    availabilitySource: "exportVerification" | "importerAssertion" | "carriedFromParentTarget";
    recordedAt: string;
    recordedBy: ReviewTargetActor;
  }>;
  limitations: string[];
}

export interface ReviewTargetPage { items: ReviewTargetSummary[]; nextBefore: number | null }
export interface ReviewTargetImportResult { outcome: "created" | "reused" | "refreshed"; reviewTarget: ReviewTargetDetail }
export interface ReviewTargetCandidates { reviewTargetId: string; startSeconds: number; endSeconds: number; operationIds: string[] }

// TV voice-review recording sessions (#222). Mirrors rcdvlog-server-core::review_sessions.
export type ReviewSessionState = "recording" | "completed" | "incomplete" | "corrupt" | "deleted";
export type RecordingEndReason = "stopped" | "deviceLost" | "interrupted" | "cancelled";
export type ReviewSessionEventKind = "manualSyncCue" | "captureDiscontinuity" | "localBufferOverflow" | "deviceLost" | "pageInterrupted" | "clockSample";

export interface CreateReviewSessionRequest {
  reviewTargetId: string;
  clientSessionKey: string;
  consent: { allPresentAcknowledged: boolean; additionalSpeakersDisclosed: boolean };
  audio: { sampleRate: number; channels: number; encoding: "pcm_s16le" };
  capture: {
    echoCancellation: boolean | null;
    noiseSuppression: boolean | null;
    autoGainControl: boolean | null;
    clock: string;
    baseLatencySeconds: number | null;
    userAgentFamily: string | null;
  };
  clientStartedPerformanceMs: number | null;
}

export interface ReviewSessionChunkUpload {
  sequence: number;
  startFrame: number;
  frameCount: number;
  sha256: string;
  captureEndMs?: number | null;
}

export interface ReviewSessionChunkReceipt {
  sessionId: string;
  sequence: number;
  startFrame: number;
  frameCount: number;
  sha256: string;
  receivedAt: string;
  duplicate: boolean;
}

export interface ReviewSessionEventRequest {
  eventId: string;
  kind: ReviewSessionEventKind;
  audioFrame: number;
  performanceMs?: number | null;
  declaredMovieSeconds?: number | null;
  detail?: Record<string, unknown> | null;
}

export interface ReviewSessionEvent {
  eventId: string;
  kind: ReviewSessionEventKind;
  audioFrame: number;
  audioSeconds: number;
  performanceMs: number | null;
  declaredMovieSeconds: number | null;
  serverReceivedAt: string;
  detail: Record<string, unknown> | null;
}

export interface ReviewSessionSummary {
  sessionId: string;
  projectId: string;
  reviewTargetId: string;
  targetDigest: string;
  revisionId: string;
  viewedArtifactId: string;
  state: ReviewSessionState;
  abandoned: boolean;
  endReason: RecordingEndReason | null;
  sampleRate: number;
  channels: number;
  encoding: string;
  createdAt: string;
  finalizedAt: string | null;
  deletedAt: string | null;
  receivedSeconds: number;
  manualCueCount: number;
}

export interface ReviewSessionDetail extends ReviewSessionSummary {
  consent: { allPresentAcknowledged: boolean; additionalSpeakersDisclosed: boolean };
  capture: CreateReviewSessionRequest["capture"];
  createdBy: ReviewTargetActor;
  clientStartedPerformanceMs: number | null;
  recordingExpiresAt: string;
  coverage: {
    receivedFrames: number;
    receivedBytes: number;
    chunkCount: number;
    highestSequence: number | null;
    missingSequences: number[];
    gaps: Array<{ startFrame: number; endFrame: number }>;
    finalFrameCount: number | null;
    receivedSeconds: number;
  };
  chunks: Array<{ sequence: number; startFrame: number; frameCount: number; byteLength: number; sha256: string; receivedAt: string; captureEndPerformanceMs: number | null }>;
  events: ReviewSessionEvent[];
  limits: { maxChunkBytes: number; maxSessionBytes: number; recordingLifetimeHours: number };
  storage: { location: string; retention: string; backup: string };
}

export interface ReviewSessionPage { items: ReviewSessionSummary[]; nextBefore: number | null }

// Review-session transcription (#224). Mirrors rcdvlog-server-core::review_transcripts.
export type ReviewTranscriptionBackend = "local" | "openrouter";
export type ReviewTranscriptionState = "queued" | "running" | "completed" | "noSpeech" | "partial" | "failed" | "cancelled";

export interface ReviewTranscriptionRequest {
  backend: ReviewTranscriptionBackend;
  model?: string | null;
  languageHint?: string | null;
  authorizedMaxCostUsd?: number | null;
  cloudUploadAcknowledged?: boolean;
  zeroDataRetention?: boolean;
}

export interface ReviewTranscriptionPreflight {
  ready: boolean;
  blockers: string[];
  warnings: string[];
  backend: ReviewTranscriptionBackend;
  provider: string;
  model: string;
  audioFingerprint: string | null;
  estimate: {
    receivedAudioSeconds: number;
    segmentCount: number;
    maxWindows: number;
    priceUsdPerSecond: number | null;
    maxCostUsd: number | null;
    billingNote: string | null;
  } | null;
  existingRunId: string | null;
}

export interface ReviewUtterance {
  utteranceId: string;
  segmentIndex: number;
  windowIndex: number;
  startFrame: number;
  endFrame: number;
  startSeconds: number;
  endSeconds: number;
  text: string;
  language: string;
  script: string;
  displayText: string;
  displayProjection: string | null;
  speaker: string;
  speakerEvidence: string;
  meanTokenProbability: number | null;
  flags: string[];
  suppressed: boolean;
}

export interface ReviewTranscriptionRun {
  schema: "review-transcript.v1";
  runId: string;
  projectId: string;
  sessionId: string;
  reviewTargetId: string;
  revisionId: string;
  backend: ReviewTranscriptionBackend;
  provider: string;
  model: string;
  recipe: string;
  settings: ReviewTranscriptionRequest;
  audioFingerprint: string;
  sampleRate: number;
  state: ReviewTranscriptionState;
  errorCode: string | null;
  attemptCount: number;
  jobId: string | null;
  createdAt: string;
  updatedAt: string;
  createdBy: ReviewTargetActor;
  segments: Array<{ segmentIndex: number; startFrame: number; endFrame: number; startSeconds: number; endSeconds: number; state: string; speechSeconds: number; windowCount: number; errorCode: string | null }>;
  utterances: ReviewUtterance[];
  recordingGaps: Array<{ startFrame: number; endFrame: number }>;
  spend: { authorizedMaxUsd: number | null; reservedUsd: number; knownCostUsd: number; unknownCostWindows: number; dispatchedWindows: number; cachedWindows: number };
  diarization: { status: string; reason: string };
  limitations: string[];
}

// Editorial review workspace (#225). Mirrors rcdvlog-server-core::review_discussions.
export type DiscussionItemKind = "observation" | "candidateEdit" | "speakerClaim" | "retraction" | "note" | "decision";
export type DecisionAction = "accepted" | "declined" | "deferred";

export interface MovieTimeEstimate { movieSeconds: number; uncertaintySeconds: number; source: string; anchorEventId: string | null }

export interface WorkspaceUtterance {
  machine: ReviewUtterance;
  text: string;
  displayText: string;
  speaker: string;
  language: string;
  excluded: boolean;
  movieTime: MovieTimeEstimate | null;
  candidateOperationIds: string[];
  referenceHint: string | null;
  corrections: Array<{ correctionId: string; field: string; value: unknown; reason: string | null; actor: ReviewTargetActor; createdAt: string }>;
}

export interface DiscussionItem {
  itemId: string;
  kind: DiscussionItemKind;
  authorKind: "human" | "agent";
  provisional: boolean;
  actor: ReviewTargetActor;
  text: string;
  speaker: string | null;
  quoteUtteranceIds: string[];
  category: string | null;
  change: Record<string, unknown> | null;
  decision: DecisionAction | null;
  refersToItemId: string | null;
  status: "open" | "accepted" | "declined" | "deferred" | "retracted";
  createdAt: string;
  canonicalNoteId: string | null;
}

export interface Discussion {
  discussionId: string;
  title: string;
  groupingSource: "heuristic" | "human";
  state: "open" | "resolved" | "deferred";
  utteranceIds: string[];
  startRecordingSeconds: number | null;
  endRecordingSeconds: number | null;
  languages: string[];
  speakers: string[];
  candidateOperationIds: string[];
  targetOperationId: string | null;
  targetSource: string | null;
  unanchored: boolean;
  items: DiscussionItem[];
  updatedAt: string;
}

export interface ReviewWorkspace {
  runId: string;
  sessionId: string;
  projectId: string;
  reviewTargetId: string;
  revisionId: string;
  viewedArtifactId: string;
  targetDigest: string;
  sync: { status: "provisional" | "unanchored" | string; source: string; note: string };
  cuts: Array<{ operationId: string; position: number; assetId: string; movieInSeconds: number; movieOutSeconds: number }>;
  utterances: WorkspaceUtterance[];
  discussions: Discussion[];
  coverage: { audiovisualReviewAttested: boolean; note: string };
}

export interface NewDiscussionItem {
  kind: DiscussionItemKind;
  authorKind: "human" | "agent";
  text: string;
  speaker?: string | null;
  quoteUtteranceIds?: string[];
  category?: string | null;
  change?: Record<string, unknown> | null;
  decision?: DecisionAction | null;
  refersToItemId?: string | null;
}

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
  listReviewTargets(projectId: string, limit?: number, before?: number): Promise<ReviewTargetPage>;
  preflightReviewTarget(projectId: string, request: ReviewTargetImportRequest): Promise<ReviewTargetPreflight>;
  importReviewTarget(projectId: string, request: ReviewTargetImportRequest): Promise<ReviewTargetImportResult>;
  getReviewTarget(projectId: string, reviewTargetId: string): Promise<ReviewTargetDetail>;
  declareReviewTargetYoutube(projectId: string, reviewTargetId: string, youtubeVideoId: string | null): Promise<ReviewTargetImportResult>;
  getReviewTargetCandidates(projectId: string, reviewTargetId: string, startSeconds: number, endSeconds: number): Promise<ReviewTargetCandidates>;
  listReviewSessions(projectId: string, reviewTargetId?: string, limit?: number, before?: number): Promise<ReviewSessionPage>;
  createReviewSession(projectId: string, request: CreateReviewSessionRequest): Promise<ReviewSessionDetail>;
  getReviewSession(projectId: string, sessionId: string): Promise<ReviewSessionDetail>;
  putReviewSessionChunk(projectId: string, sessionId: string, chunk: ReviewSessionChunkUpload, bytes: Uint8Array): Promise<ReviewSessionChunkReceipt>;
  recordReviewSessionEvent(projectId: string, sessionId: string, event: ReviewSessionEventRequest): Promise<ReviewSessionEvent>;
  finalizeReviewSession(projectId: string, sessionId: string, request: { finalFrameCount: number; endReason: RecordingEndReason }): Promise<ReviewSessionDetail>;
  deleteReviewSession(projectId: string, sessionId: string): Promise<ReviewSessionDetail>;
  reviewSessionAudioUrl(projectId: string, sessionId: string): string;
  preflightReviewTranscription(projectId: string, sessionId: string, request: ReviewTranscriptionRequest): Promise<ReviewTranscriptionPreflight>;
  startReviewTranscription(projectId: string, sessionId: string, request: ReviewTranscriptionRequest): Promise<ReviewTranscriptionRun>;
  listReviewTranscriptions(projectId: string, sessionId: string): Promise<{ items: ReviewTranscriptionRun[] }>;
  getReviewTranscription(projectId: string, runId: string): Promise<ReviewTranscriptionRun>;
  cancelReviewTranscription(projectId: string, runId: string): Promise<ReviewTranscriptionRun>;
  getReviewWorkspace(projectId: string, runId: string): Promise<ReviewWorkspace>;
  getReviewReport(projectId: string, runId: string): Promise<Record<string, unknown>>;
  reviewReportUrl(projectId: string, runId: string): string;
  proposeReviewDiscussions(projectId: string, runId: string): Promise<ReviewWorkspace>;
  createReviewDiscussion(projectId: string, runId: string, title: string, utteranceIds: string[]): Promise<ReviewWorkspace>;
  correctReviewUtterance(projectId: string, runId: string, utteranceId: string, field: "text" | "speaker" | "language" | "movieTime" | "excluded", value: unknown, reason?: string | null): Promise<ReviewWorkspace>;
  updateReviewDiscussion(projectId: string, discussionId: string, update: { title?: string; utteranceIds?: string[]; targetOperationId?: string | null; unanchored?: boolean; state?: "open" | "resolved" | "deferred" }): Promise<ReviewWorkspace>;
  addReviewDiscussionItem(projectId: string, discussionId: string, item: NewDiscussionItem): Promise<ReviewWorkspace>;
  reconcileReviewDiscussionItem(projectId: string, itemId: string, canonicalNoteId: string): Promise<DiscussionItem>;
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
