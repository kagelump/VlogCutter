// Types for the calibration, diagnostics, and edit-plan surfaces
// (D01-T015, D01-T016, D01-T023, and the D01-T027 diagnostics consumer).
//
// These mirror the Rust types exactly. Nothing here re-derives a chronology
// value: the desktop displays what the solver decided, because the desktop and
// the CLI must never be able to disagree about order.

export interface CaptureSequence {
  id: string;
  cameraKey: string;
  label: string;
  volumeHint: string;
  filenamePrefix: string;
}

export interface ClockEpoch {
  id: string;
  captureSequenceId: string;
  startCounter: number;
  /** null means open-ended. */
  endCounter: number | null;
  offsetSeconds: number;
  confidence: number;
  evidence: string[];
  label: string;
}

export interface ClipTimeException {
  assetId: string;
  tripTime: string;
  reason: string;
}

export type PhaseKind = "location" | "transit";
export type DisplayMode = "local" | "dual" | "utc";
export type PhaseSplitDirection = "through" | "from";

export interface TripPhase {
  id: string;
  label: string;
  kind: PhaseKind;
  startAnchorAssetId: string | null;
  endAnchorAssetId: string | null;
  startTripTime: string;
  endTripTime: string;
  timezone: string | null;
  originTimezone: string | null;
  destinationTimezone: string | null;
  displayMode: DisplayMode;
}

export type ConstraintStrength = "hard" | "soft";

export interface EventOrderConstraint {
  id: string;
  earlierAssetId: string;
  laterAssetId: string;
  strength: ConstraintStrength;
  source: string;
}

export interface DisplayTime {
  primary: string;
  secondary: string | null;
  timezone: string;
  phaseId: string | null;
}

export interface Evidence {
  kind: string;
  detail: string;
  weight: number;
}

export interface OrderKey {
  tripTimeMillis: number | null;
  captureSequenceId: string;
  filenameCounter: number | null;
  filename: string;
  assetId: string;
}

export interface AssetChronology {
  assetId: string;
  rawTime: string | null;
  tripTime: string | null;
  displayTime: DisplayTime | null;
  confidence: number;
  evidence: Evidence[];
  uncertaintySeconds: number;
  orderKey: OrderKey;
}

export type ContradictionKind =
  | "cycle"
  | "interval_conflict"
  | "exception_conflict"
  | "phase_anchor_conflict";

export interface Contradiction {
  kind: ContradictionKind;
  strength: ConstraintStrength;
  assetIds: string[];
  evidence: Evidence[];
  message: string;
}

export type AppliedCorrection =
  | { type: "captureSequence"; sequenceId: string; label: string }
  | { type: "clockEpoch"; epochId: string; offsetSeconds: number; label: string }
  | { type: "clipException"; tripTime: string; reason: string }
  | { type: "tripPhase"; phaseId: string; timezone: string; label: string }
  | { type: "eventOrder"; constraintId: string; strength: ConstraintStrength };

export interface ChronologyExplanation {
  assetId: string;
  rawTime: string | null;
  tripTime: string | null;
  displayTime: DisplayTime | null;
  confidence: number;
  uncertaintySeconds: number;
  appliedCorrections: AppliedCorrection[];
  contradictions: Contradiction[];
}

export interface ProposedSplit {
  captureSequenceId: string;
  atCounter: number;
  reason: string;
  affectedAssetIds: string[];
}

/** A counter restart the user has answered "same card" to. */
export interface DeclinedSplit {
  captureSequenceId: string;
  atCounter: number;
  reason: string;
  declinedAt: string;
}

export interface ChronologyOverview {
  sequences: CaptureSequence[];
  epochs: ClockEpoch[];
  exceptions: ClipTimeException[];
  phases: TripPhase[];
  constraints: EventOrderConstraint[];
  contradictions: Contradiction[];
  assets: AssetChronology[];
  proposedSplits: ProposedSplit[];
  /** Boundaries already answered "same card"; listed so the answer is reversible. */
  declinedSplits: DeclinedSplit[];
  unplacedAssets: number;
}

export interface ChronologyImpactChange {
  assetId: string;
  before: string | null;
  after: string | null;
  beforePosition: number;
  afterPosition: number;
}

export interface ChronologyImpact {
  changes: ChronologyImpactChange[];
  contradictions: Contradiction[];
  blocked: boolean;
}

export interface ClockEpochImpactPreview {
  epoch: ClockEpoch;
  factsToken: string;
  impact: ChronologyImpact;
}

export interface ClockEpochClearPreview {
  epochId: string;
  factsToken: string;
  impact: ChronologyImpact;
}

export interface VisualPlacementPreview {
  epoch: ClockEpoch;
  factsToken: string;
  impact: ChronologyImpact;
}

export interface CaptureSequenceReassignmentPreview {
  fromSequenceId: string;
  intoSequenceId: string;
  assetIds: string[];
  factsToken: string;
  impact: ChronologyImpact;
}

export interface CaptureSequenceSplitPreview {
  sourceSequenceId: string;
  atCounter: number;
  createdSequence: CaptureSequence;
  assetIds: string[];
  factsToken: string;
  impact: ChronologyImpact;
}

export interface CaptureSequenceMergePreview {
  fromSequenceId: string;
  intoSequenceId: string;
  assetIds: string[];
  epochIds: string[];
  factsToken: string;
  impact: ChronologyImpact;
}

/** A place in the review queue that survives a reorder. */
export interface QueuePosition {
  anchorAssetId: string;
  orderedPredecessors: string[];
}

export type DiagnosticSeverity = "error" | "warning" | "info";

export type DiagnosticSubject =
  | { type: "project" }
  | { type: "asset"; assetId: string }
  | { type: "operation"; operationId: string }
  | { type: "volume"; volumeHint: string }
  | { type: "captureSequence"; captureSequenceId: string };

export interface Diagnostic {
  id: string;
  dedupeKey: string;
  severity: DiagnosticSeverity;
  kind: string;
  subject: DiagnosticSubject;
  message: string;
  context: unknown;
  occurrenceCount: number;
  firstSeenAt: string;
  lastSeenAt: string;
  acknowledgedAt: string | null;
}

export interface DiagnosticCounts {
  errors: number;
  warnings: number;
  infos: number;
}

export interface DiagnosticsView {
  diagnostics: Diagnostic[];
  counts: DiagnosticCounts;
}

export interface EditPlanOperation {
  operationId: string;
  position: number;
  assetId: string;
  sourceInSeconds: number;
  sourceOutSeconds: number;
  rationale: string;
  audio?: EditPlanAudio;
}

export type EditPlanAudio =
  | { mode: "silent" }
  | {
      mode: "source";
      version: number;
      assetId: string;
      sourceInSeconds: number;
      sourceOutSeconds: number;
      timelineStartSeconds: number;
      syncOffsetSeconds: number;
      gainDb?: number;
      fadeInSeconds?: number;
      fadeOutSeconds?: number;
    };

export interface EditPlanOperationInput {
  operationId?: string;
  assetId: string;
  sourceInSeconds: number;
  sourceOutSeconds: number;
  rationale: string;
  audio?: EditPlanAudio;
}

export interface EditPlanRevision {
  revisionId: string;
  projectId: string;
  scriptRevisionId: string | null;
  parentRevisionId: string | null;
  active: boolean;
  operations: EditPlanOperation[];
  createdAt: string;
}

export type FindingSeverity = "error" | "warning";

export interface Finding {
  kind: string;
  severity: FindingSeverity;
  code: string;
  position: number | null;
  assetId: string | null;
  message: string;
}

export interface ValidationReport {
  findings: Finding[];
}

export interface PreviewResult {
  outputPath: string;
  durationSeconds: number;
  clipCount: number;
  failures: { position: number; assetId: string; message: string }[];
}

export type PreviewProgress =
  | { type: "started"; clipCount: number; workerCount: number }
  | {
      type: "clipCompleted";
      position: number;
      elapsedSeconds: number;
      estimateRemainingSeconds: number | null;
    }
  | { type: "clipFailed"; position: number; message: string }
  | { type: "finished"; outputPath: string; durationSeconds: number }
  | { type: "cancelled" };

export interface OtioExportResult {
  outputPath: string;
  outputDigest: string;
  clipCount: number;
}
