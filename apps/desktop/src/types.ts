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

/**
 * A kind of regenerable local data (#16). Every one of these can be made again;
 * what differs is what it costs and what stops working until it is.
 */
export type CacheKind =
  | "proxies"
  | "thumbnails"
  | "audio"
  | "framePacks"
  | "editPreviews"
  | "importPreviews"
  | "analysisModels";

/** Whether a cache belongs to one project or to the catalog holding it. */
export type CacheScope = "project" | "catalog";

export interface CacheCategory {
  kind: CacheKind;
  scope: CacheScope;
  bytes: number;
  fileCount: number;
}

export interface ProjectStorageReport {
  projectId: string;
  catalogPath: string;
  /** The user's own work: annotations, chronology, import history. */
  catalogBytes: number;
  categories: CacheCategory[];
  cacheBytes: number;
}

export interface CacheCleanupReport {
  removedBytes: number;
  removedFiles: number;
  failures: Array<{ path: string; error: string }>;
}

/**
 * Whether a project's folder can be removed, and every reason it cannot.
 * A folder holding an archive root, a referenced original, another project, or
 * any file VlogCutter did not create is never removable.
 */
export interface ProjectRemovalAudit {
  projectId: string;
  projectName: string;
  catalogPath: string;
  folderPath: string;
  catalogBytes: number;
  cacheBytes: number;
  /** Clips whose membership ends. Not one byte of their media is touched. */
  assetCount: number;
  otherProjectsInCatalog: string[];
  archiveRootsInside: string[];
  originalsInside: number;
  unexpectedEntries: string[];
  folderRemovable: boolean;
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
  /** Verified camera supplied `.LRF` sidecar is available for review. */
  cameraProxyAvailable?: boolean;
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
  /** The operation revision this view was materialized from. */
  revision: number;
  phase?: string | null;
  location?: string | null;
  timezone?: string | null;
  storyBeat?: string | null;
}

// ── Clip analysis ────────────────────────────────────────────────────

export type AnalysisStage = "proxies" | "transcripts" | "analysis";

export type TranscriptStatus =
  | "speech"
  | "no_speech"
  | "no_audio"
  | "no_accepted_speech"
  | "rejected"
  | "failed"
  | "review_required";

export type TranscriptOutcomeReason =
  | "accepted"
  | "no_audio_stream"
  | "no_detected_speech"
  | "empty_provider_output"
  | "quality_rejected"
  | "processing_failed"
  | "legacy_no_speech_unverified"
  | "quality_evidence_incomplete"
  | "native_coverage_incomplete";

export type CameraMovement = "static" | "pan" | "tilt" | "handheld" | "tracking" | "zoom" | "other";

export type UnusableReason = "black" | "blurry" | "noise" | "overexposed" | "other";

export interface TranscriptSegment {
  startMs: number;
  endMs: number;
  language: string;
  text: string;
  /**
   * Observed arithmetic mean token probability from the local decoder, or
   * `null` when comparable token evidence is absent. `null` is not confidence
   * zero/one and must not be rendered or compared as one.
   */
  meanTokenProbability: number | null;
}

export interface ClipTranscript {
  assetId: string;
  recipeKey: string;
  sourceFingerprint: string;
  /**
   * Terminal outcome. `no_speech` means the VAD gate ran and found no speech;
   * `no_audio` means the media had no audio stream; `no_accepted_speech` means
   * speech was detected but the provider returned no usable output.
   * `review_required` retains timed candidate text with incomplete quality
   * evidence for human review only; it is not accepted speech.
   */
  status: TranscriptStatus;
  outcomeReason: TranscriptOutcomeReason;
  rejectReason: string;
  primaryLanguage: string;
  /** Detected speech duration, retained even when no transcript was accepted. */
  speechSeconds: number;
  /** Provider decode windows attempted. */
  windowCount: number;
  /** Raw decoded segments before filtering and quality gates. */
  providerSegmentCount: number;
  meanTokenProbability: number | null;
  repetitionRatio: number | null;
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
  attemptedModels: string[];
  usageComplete: boolean;
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
  role: "speech_to_text" | "voice_activity_detection" | string;
  installed: boolean;
  ready: boolean;
  present: boolean;
  integrity: "verified" | "present_unverified" | "invalid" | "missing" | string;
  detail: string | null;
  bytes: number;
  url: string;
  directory: string;
  path: string;
  expectedBlake3: string | null;
}

export interface ModelInstallProgress {
  name: string;
  downloadedBytes: number;
  totalBytes: number;
}

export interface AnalysisRunRecord {
  accountingContract: string;
  id: string;
  projectId: string;
  startedAt: string;
  finishedAt: string;
  status: "running" | "completed" | "cancelled" | "failed";
  stageReached: AnalysisStage | "moments";
  policyId: string;
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
  reachableOriginals: number;
  unverifiedVideoClips: number;
  quarantinedVideoClips: number;
  unavailableVideoClips: number;
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
  transcripts: {
    transcribed: number;
    noAudio: number;
    failed: number;
    skipped: number;
    cancelled: boolean;
    failures: Array<{ path: string; error: string }>;
  };
  analysis: {
    analyzed: number;
    skipped: number;
    rejected: number;
    fallbackServed: number;
    costUsd: number;
    usageComplete: boolean;
    cancelled: boolean;
    budgetExhausted: boolean;
    failures: Array<{ path: string; error: string }>;
  };
  costUsd: number;
}

export interface AnalysisRunRequest {
  force?: boolean;
  budgetUsd?: number | null;
  primaryModel?: string | null;
  allowedModels?: string[];
  allowFallback?: boolean | null;
}

export interface ChronologySuggestionRequest {
  projectId: string;
  assetIds: string[];
  primaryModel: string;
  allowedModels: string[];
  allowFallback: boolean;
  /** Finite ceiling for the shared budget authority. Required unless the environment supplies one. */
  budgetUsd?: number | null;
}

export interface ChronologySuggestionResult {
  suggestion: { frameIds: string[]; matches: Array<{ earlierAssetId: string; laterAssetId: string; supportingFrameIds: string[] }> };
  review: { missingFrameIds: string[]; duplicateFrameIds: string[]; unknownFrameIds: string[]; semanticInconsistencies: string[] };
  provenance: {
    model: string;
    attempts: number;
    attemptedModels: string[];
    usageComplete: boolean;
    costUsd: number;
    uncertainUsageReserveUsd?: number;
    activeReservedCostUsd?: number;
    committedOrReservedUsd?: number;
    actualProvider?: string | null;
    actualModel?: string | null;
    providerGenerationId?: string | null;
    runId?: string;
    requestKeys?: string[];
    promptTokens: number;
    completionTokens: number;
    latencyMs: number;
  };
  cached: boolean;
}

export interface AssetPatch {
  notes: string;
  labels: string[];
  removed: boolean;
  rotationDegrees: number;
  /**
   * The `Asset.revision` this patch was decided from. The backend refuses the
   * write when the clip has moved since, because a whole patch carries every
   * judgment field and applying a stale one reverses the other writer rather
   * than merely missing it.
   */
  expectedRevision?: number | null;
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
  /**
   * The volume this root was bound to on this machine (#78). Null for a
   * binding made before volumes had identity, or for a volume that cannot
   * describe itself — such a root still resolves by path, it just cannot prove
   * the drive that answered is the drive it was bound to.
   */
  volumeId: string | null;
  createdAt: string;
}

/**
 * One project's durable "everything before this clip is old" boundary for one
 * card session (#66). Keyed on the card session rather than a mount path or a
 * camera, so it survives remounts, resets on reformat, and never arrives
 * pre-applied to a brand-new card.
 */
export interface ImportCutoff {
  id: string;
  cardSessionId: string;
  cardLabel: string;
  /** The boundary: card-relative path of the clip the user pointed at. */
  relativePath: string;
  filename: string;
  capturedAt: string | null;
  createdAt: string;
  updatedAt: string;
}

export type ImportClassification =
  | "new"
  | "knownVerified"
  | "knownRelinkable"
  | "knownMissingProxy"
  | "pathCollision"
  | "unreadable"
  /**
   * Older than this project's stored "start here" boundary for this card
   * (#66). Not copied, not ingested, not proxied, and deliberately not a
   * failure — the user said this footage predates the project.
   */
  | "excludedByCutoff";

export interface PreflightItem {
  path: string;
  relativePath: string;
  filename: string;
  sizeBytes: number;
  classification: ImportClassification;
  assetId: string | null;
  error: string;
  /**
   * The sampled content fingerprint already spent to classify this file.
   * Reused as the import-preview poster cache key. Absent when the file
   * could not be read well enough to classify (`unreadable`).
   */
  fingerprint?: string | null;
  /**
   * The file's own filesystem modification time (RFC 3339), read from the
   * same `stat` preflight already pays for every candidate. This is a
   * best-available stand-in for a probed capture time, not a claim about
   * when the footage was actually shot — preflight never runs ffprobe.
   * `ImportPreviewItem.capturedAt`, and after import the file's own real
   * `timestamp` in `ImportRunReport.files`, both supersede it.
   */
  filesystemModifiedAt?: string | null;
}

/** One file `generateImportPreview` was asked for. */
export interface ImportPreviewRequest {
  path: string;
  fingerprint?: string | null;
}

/**
 * What import-preview could determine for one file, without mutating
 * anything. Any field being absent degrades only that field on its row;
 * `error` explains why, but a row missing its poster or capture time is
 * still an otherwise-importable file.
 */
export interface ImportPreviewItem {
  path: string;
  thumbnailPath: string | null;
  durationSeconds: number | null;
  /** The camera's own probed capture time, distinct from the filesystem mtime. */
  capturedAt: string | null;
  error: string | null;
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

export type ImportSequenceBoundaryEvidence =
  | { kind: "volumeChange"; from: string; to: string }
  | { kind: "counterReset"; previous: number; next: number }
  | { kind: "folderReset"; from: string; to: string }
  | { kind: "prefixChange"; from: string; to: string }
  | { kind: "largeDiscontinuity"; gapSeconds: number };

export interface ImportSequenceBoundary {
  beforeRelativePath: string;
  afterRelativePath: string;
  affectedClipCount: number;
  evidence: ImportSequenceBoundaryEvidence[];
}

export type ImportSequenceRunDecision = "extendExisting" | "proposeNew";

export interface ImportSequenceRunPreview {
  firstRelativePath: string;
  lastRelativePath: string;
  clipCount: number;
  cameraKey: string;
  volumeHint: string;
  filenamePrefix: string;
  decision: ImportSequenceRunDecision;
  matchedExistingSequenceId: string | null;
  matchedExistingSequenceLabel: string | null;
  comparedExistingSequenceId: string | null;
  comparedExistingSequenceLabel: string | null;
  /**
   * True only when this run's own evidence actively disagrees with
   * `comparedExistingSequenceId` (card/filename evidence differs). A
   * `proposeNew` run can also carry a `comparedExistingSequenceId` because
   * another incoming run already claimed that sequence, or because a
   * boundary split it off deliberately — both are routine, expected
   * outcomes of the sequencing algorithm, not something the user needs to
   * look at before import. Use this field to tell "genuinely ambiguous"
   * apart from "routine new sequence" — never pattern-match `reason`, which
   * is prose for humans and can be reworded without notice.
   */
  needsReview: boolean;
  reason: string;
  evidence: ImportSequenceBoundaryEvidence[];
}

export interface ImportSequencePreview {
  newClipCount: number;
  existingSequenceCount: number;
  proposedSequenceCount: number;
  matchedExistingSequenceCount: number;
  suggestedNewSequenceCount: number;
  boundaries: ImportSequenceBoundary[];
  runs: ImportSequenceRunPreview[];
}

/** Where a plan's camera identity came from (#71). */
export type ImportCameraSource = "userSelected" | "remembered" | "detected" | "unknown";

/**
 * The one camera identity a plan resolved to. This flows unchanged into both
 * the sequence preview below and every new asset the import creates, so the
 * two can never disagree about which camera this footage belongs to. See
 * `ImportDialog`'s camera control for how `source` drives what the dialog
 * shows the user.
 */
export interface ResolvedImportCamera {
  camera: string;
  source: ImportCameraSource;
  /**
   * Whether this import's folder layout, filename prefix, and representative
   * EXIF make/model were all present, making the evidence safe to remember.
   * When false, `source` is never `"remembered"` — present `camera` as a
   * choice for this run, not a settled future default.
   */
  evidenceStrong: boolean;
}

/** The durable evidence used to recall a user-selected camera on a later card. */
export interface CameraEvidence {
  folderHint: string;
  filenamePrefix: string;
  exifHint: string;
}

export interface ImportPreflight {
  sourcePath: string;
  card: CardCandidate | null;
  /** Optional only for compatibility with older CUJ fixtures. Production preflight always sends it. */
  cameraIdentity?: ResolvedImportCamera;
  /** Optional only for compatibility with older CUJ fixtures. Production preflight always sends it. */
  cameraEvidence?: CameraEvidence;
  destination: ImportDestination;
  scanned: number;
  newClips: number;
  knownClips: number;
  relinkable: number;
  missingProxies: number;
  collisions: number;
  unreadable: number;
  /** This project's stored "start here" boundary for this card, if it has one (#66). */
  cutoff?: ImportCutoff | null;
  /** Candidates the cutoff excluded — deliberately never folded into `knownClips`. */
  excludedByCutoff?: number;
  /**
   * Clips before the cutoff whose original this project has lost and which
   * this source could repair. The cutoff never suppresses a relink, and this
   * count is how the dialog says so rather than leaving a surprise in a
   * filtered list.
   */
  relinkableBeforeCutoff?: number;
  /** Optional only for compatibility with older CUJ fixtures. Production preflight always sends it. */
  sequencePreview?: ImportSequencePreview;
  items: PreflightItem[];
}

/** One stored evidence-to-camera correction, inspectable and editable per project. */
export interface CameraMapping {
  id: string;
  folderHint: string;
  filenamePrefix: string;
  /**
   * Raw EXIF make/model this row was recorded with. Empty only on a legacy
   * migration-21 row, which is retained for inspection but is too ambiguous
   * to apply automatically. Not user-editable.
   */
  exifHint: string;
  camera: string;
  createdAt: string;
  updatedAt: string;
}

export interface CameraMergePreview {
  projectId: string;
  sourceCamera: string;
  destinationCamera: string;
  affectedClipCount: number;
  affectedCaptureSequenceCount: number;
  affectedMappingCount: number;
  destinationCaptureSequenceCount: number;
}

export interface CameraMergeReport {
  projectId: string;
  sourceCamera: string;
  destinationCamera: string;
  movedClipCount: number;
  movedCaptureSequenceCount: number;
  movedMappingCount: number;
}

export type ImportStage = "scanning" | "copying" | "reading" | "proxies" | "complete";

export interface ImportProgress {
  /** The project that owns this progress stream; stale-project events are ignored. */
  projectId: string;
  stage: ImportStage;
  completed: number;
  total: number;
  currentFile: string;
  completedBytes: number;
  totalBytes: number;
  /** Present only once this file's catalog membership is safely committed. */
  asset?: Asset | null;
}

export interface ImportOutcome {
  imported: number;
  importedAssetIds: string[];
  relinked: number;
  relinkedAssetIds: string[];
  skipped: number;
  copied: number;
  bytesCopied: number;
  cancelled: boolean;
  /**
   * The run stopped because the source volume went away underneath it (#67).
   * Not a cancellation and not a per-file failure: committed clips stayed
   * committed, and resuming copies only the remainder.
   */
  sourceDisconnected?: boolean;
  /**
   * The run stopped because the archive destination went away underneath it
   * (#76). The mirror of `sourceDisconnected`: committed clips stayed
   * committed, and resuming copies only the remainder.
   */
  destinationDisconnected?: boolean;
  failures: Array<{ path: string; error: string }>;
  /** A committed clip needs optional bookkeeping attention; it is not a failed import. */
  warnings?: Array<{ path: string; error: string }>;
  /** Import succeeded, but its explicit camera choice could not be remembered. */
  cameraMappingWarning?: string | null;
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
  session?: ImportSessionDetail;
}

export interface ImportHistoryEntry {
  id: string; projectId: string; sourcePath: string; destinationMode: "reference" | "archive_copy";
  destinationRootId: string | null; destinationName: string; destinationPath: string;
  imported: number; relinked: number; skipped: number; copied: number; bytesCopied: number;
  cancelled: boolean; failures: Array<{ path: string; error: string }>;
  rolledBackAt: string | null; rolledBackAssetCount: number; createdAt: string;
}
export interface ImportSessionFile { assetId: string | null; path: string; filename: string; classification: string; result: string; timestamp: string | null; durationSeconds: number | null; error: string | null; }
export interface ImportSessionDetail extends ImportHistoryEntry { files: ImportSessionFile[]; }
export interface ImportRollbackPreview { sessionId: string; removableAssetCount: number; alreadyRemovedAssetCount: number; modifiedClips: Array<{ assetId: string; filename: string; reasons: string[] }>; affectedPhaseCount: number; message: string; }

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
  confidence?: string;
  estimatedRemainingSecondsRange?: [number, number] | null;
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
