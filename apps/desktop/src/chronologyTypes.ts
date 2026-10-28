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

export type ReviewAuthor = "human" | "reviewer";
export type ReviewCategory = "endpoint" | "order" | "duration" | "featuredShot" | "soundContinuity" | "intent" | "preference";
export interface ReviewCutSnapshot { operationId: string; position: number; assetId: string; sourceInSeconds: number; sourceOutSeconds: number; audio: EditPlanAudio | null }
export interface ReviewSourceReference { assetId: string; sourceFingerprint: string; representationFingerprint: string; rotationDegrees: number }
export interface ReviewMovieEvidence { artifactId: string; outputFingerprint: string; movieTimeBaseNumerator: number; movieTimeBaseDenominator: number; movieDurationSeconds: number; requestedMovieSeconds: number | null; timestampMapping: string; pictureMapping: PreviewCutMapping; sourceFingerprints: ReviewSourceReference[]; limitations: string[] }
export interface ReviewTarget { revisionId: string; cut: ReviewCutSnapshot; previousCut: ReviewCutSnapshot | null; nextCut: ReviewCutSnapshot | null; sourceOccurrences: number; contextAudioDigest: string; contextAudioPrecision: string; contextAudioEventCount: number; viewedMovie: ReviewMovieEvidence | null }
export interface RecordReviewNoteRequest { projectId: string; revisionId: string; operationId: string; author: ReviewAuthor; category: ReviewCategory; observation: string; viewedArtifactId?: string | null; movieTimeSeconds?: number | null; evidenceRefs?: string[] }
export interface ReviewNote { noteId: string; projectId: string; author: ReviewAuthor; category: ReviewCategory; observation: string; target: ReviewTarget; evidenceRefs: string[] }
export type ReviewChange = { kind: "trim"; sourceInSeconds: number; sourceOutSeconds: number } | { kind: "replace"; assetId: string; sourceInSeconds: number; sourceOutSeconds: number; audio: EditPlanAudio | null } | { kind: "reorder"; beforeOperationId: string | null } | { kind: "omit" } | { kind: "audio"; audio: EditPlanAudio | null };
export interface ProposeReviewChangeRequest { noteId: string; expectedRevision: number; author: ReviewAuthor; change: ReviewChange; rationale: string }
export interface ReviewProposal { proposalId: string; noteId: string; author: ReviewAuthor; target: ReviewTarget; change: ReviewChange; rationale: string }
export type ReviewDecisionAction = "accepted" | "deferred" | "superseded" | "addressed";
export type ReviewDecisionState = ReviewDecisionAction | "applied";
export type ReviewDecisionOrigin = "human" | "host";
export type ReviewHardChoice = "keepPicture" | "keepPictureAndAudio";
export interface DecideReviewFeedbackRequest { noteId: string; expectedRevision: number; proposalId: string | null; decisionOwner: RangeDecisionOwner; action: ReviewDecisionAction; rationale: string; hardChoice: ReviewHardChoice | null }
export interface ReviewDecision { decisionOrigin: ReviewDecisionOrigin; decisionId: string; noteId: string; proposalId: string | null; state: ReviewDecisionState; rationale: string; hardChoice: ReviewHardChoice | null; retainedTarget: ReviewTarget | null; appliedRevisionId: string | null; acceptedDecisionId: string | null }
export interface MapReviewTargetRequest { noteId: string; expectedRevision: number; proposalId: string | null; revisionId: string; operationId: string; decisionOwner: RangeDecisionOwner; rationale: string }
export interface ReviewTargetMapping { decisionOwner: RangeDecisionOwner; mappingId: string; noteId: string; proposalId: string | null; target: ReviewTarget; rationale: string }
export type ReviewFeedbackFact = ({ kind: "note" } & ReviewNote) | ({ kind: "proposal" } & ReviewProposal) | ({ kind: "decision" } & ReviewDecision) | ({ kind: "mapping" } & ReviewTargetMapping);
export interface ReviewFeedbackHistoryItem { operationId: string; noteRevision: number; recordedAtMillis: number; fact: ReviewFeedbackFact }
export interface ReviewTargetStatus { flags: string[]; candidateOperationIds: string[]; candidatesTruncated: boolean; requiresMapping: boolean }
export interface ReviewFeedbackView { note: ReviewNote; revision: number; currentRevisionId: string; effectiveTarget: ReviewTarget; targetStatus: ReviewTargetStatus; latestProposal: ReviewProposal | null; latestDecision: ReviewDecision | null; decidedProposal: ReviewProposal | null; proposalTargetStatus: ReviewTargetStatus | null; resolution: string; history: ReviewFeedbackHistoryItem[]; historyTotal: number; historyTruncated: boolean }
export interface ReviewFeedbackPage { schemaReady: boolean; projectId: string; revisionId: string; total: number; nextOffset: number | null; items: ReviewFeedbackView[] }
export interface AcceptedReviewReference { noteId: string; expectedRevision: number; proposalId: string }
export interface ApplyReviewFeedbackRequest { projectId: string; baseRevisionId: string; expectedActiveRevisionId: string | null; accepted: AcceptedReviewReference[] }
export interface EditPlanChange { operationId: string; changes: string[]; before: EditPlanOperation | null; after: EditPlanOperation | null }
export interface EditPlanDiff { baseRevisionId: string; revisionId: string; changes: EditPlanChange[] }
export interface ApplyReviewFeedbackResult { revision: EditPlanRevision; diff: EditPlanDiff; validation: ValidationReport; addressedNoteIds: string[] }

export type SelectionActivity = "active" | "passive" | "movement" | "dialogue" | "uncertain";
export type SelectionMomentSource =
  | { kind: "current"; assetId: string; expectedMomentIdentity: string; candidateIds?: string[] }
  | { kind: "declared"; assetId: string; expectedSourceFingerprint: string; rationale: string; candidates: MomentCandidate[] };
export interface MomentSpan { startMs: number; endMs: number }
export interface MomentCandidate { id: string; rank: number; score: number; core: MomentSpan; payoff: MomentSpan; setup: MomentSpan | null; context: MomentSpan | null; allowedRange: MomentSpan; reason: string; event: string; role: string; evidence: Array<{ kind: string; key: string; span: MomentSpan | null }>; uncertainty: string[]; dialogueBoundaries: MomentSpan[]; refined: boolean; longHold?: boolean }
export interface SelectionCandidateKey { sequenceId: string; assetId: string; candidateId: string }
export interface SequencePacingProfile { preset: "natural" | "rapidMovement" | "custom"; preferredMinSeconds: number; preferredMaxSeconds: number; longReviewSeconds: number; preferredUpperSeconds: number; preferredLongHolds: number; allowMontageFragments: boolean }
export interface SequenceIntent { sequenceId: string; goal: string; pacing: SequencePacingProfile; sources: SelectionMomentSource[]; roleClaims?: Array<{ key: SelectionCandidateKey; role: string; activity: SelectionActivity; rationale: string }>; retainedChoices?: Array<{ noteId: string; role: string; activity: SelectionActivity; rationale: string }>; heroCandidates?: SelectionCandidateKey[]; budgetSeconds: number | null; hardBudget?: boolean; majorPlaceChange?: boolean; placeContext: { kind: "visual" | "spoken" | "humanSupplied"; description: string; establishingCandidate: SelectionCandidateKey; decisionOwner: "human" } | null }
export interface SequenceSelectionInput { version: number; decisionOwner: "human"; goal: string; sequences: SequenceIntent[]; include?: SelectionCandidateKey[]; exclude?: SelectionCandidateKey[]; counts?: Array<{ sequenceId: string | null; role: string; minimum: number; maximum: number; hard: boolean; rationale: string }>; relationships?: Array<{ before: SelectionCandidateKey; after: SelectionCandidateKey; keepTogether: boolean; hard: boolean; rationale: string }>; budgetSeconds: number | null; hardBudget?: boolean; minimumScore: number; repetitionPenalty: number; coverageBonus: number }
export interface SequenceSelectionRequest { projectId: string; expectedBaseRevisionId: string | null; expectedActiveRevisionId: string | null; input: SequenceSelectionInput }
export interface SelectionEvidenceIdentity { sourceFingerprint: string; momentIdentity: string; candidateFingerprint: string; origin: string; provider: string | null; requestedModel: string | null; actualModel: string | null; promptVersion: string | null; schemaVersion: string | null; recipeVersion: string; policyFingerprint: string; representationFingerprint: string | null; samplingLimits: string[] }
export interface SelectedMomentAssignment { operationId: string; sequenceId: string; candidate: SelectionCandidateKey | null; retainedNoteId: string | null; assetId: string; role: string; activity: SelectionActivity; event: string; coverageBucket: number; originalCore: MomentSpan; completeSpan: MomentSpan; chosenSourceInSeconds: number; chosenSourceOutSeconds: number; montageFragment: boolean; rationale: string; evidence: SelectionEvidenceIdentity; uncertainty: string[] }
export interface SelectionConflict { candidate: SelectionCandidateKey | null; assetId: string | null; code: string; blocking: boolean; sequenceId: string | null; operationId: string | null; message: string }
export interface SelectionMetadata { version: number; recipe: string; inputIdentity: string; input: SequenceSelectionInput; assignments: SelectedMomentAssignment[]; constraintsKey: string; planningConflicts: SelectionConflict[] }
export interface SelectionRoleDistribution { role: string; count: number; durationSeconds: number; shortestSeconds: number; longestSeconds: number; medianSeconds: number }
export interface SelectionSequenceSummary { sequenceId: string; goal: string; earnedSeconds: number; preferredBudgetSeconds: number | null; unfilledSeconds: number | null; activeSeconds: number; passiveSeconds: number; uncertainSeconds: number; earlyCount: number; middleCount: number; lateCount: number; roles: SelectionRoleDistribution[] }
export interface SelectionReviewItem { operationId: string; sequenceId: string; durationSeconds: number; reason: string; alternatives: SelectionCandidateKey[] }
export interface SelectionReport { durationBasis: string; recipe: string; inputIdentity: string; reused: boolean; earnedSeconds: number; preferredBudgetSeconds: number | null; unfilledSeconds: number | null; sequences: SelectionSequenceSummary[]; conflicts: SelectionConflict[]; longTakeReview: SelectionReviewItem[]; deferredCandidates: SelectionCandidateKey[]; limitations: string[] }
export interface SelectionProposal { operations: Array<EditPlanOperationInput & { position: number }>; metadata: SelectionMetadata; report: SelectionReport }
export interface SelectionDraftResult { revision: EditPlanRevision; report: SelectionReport; validation: ValidationReport }
export interface SelectionRevisionView { schemaReady: boolean; revisionId: string; metadata: SelectionMetadata | null; report: SelectionReport | null }

export type InspectionRepresentation = "original" | "generatedProxy" | "cameraProxy";
export type InspectionPurpose = "locator" | "proposedBoundary" | "inspectedBoundary";

/** Input for one bounded source-window evidence request. Fields with defaults
 * in Rust may be omitted by callers. */
export interface SourceInspectionRequest {
  assetId: string;
  representation: InspectionRepresentation;
  purpose: InspectionPurpose;
  sourceInSeconds: number;
  sourceOutSeconds: number;
  beforeSeconds?: number;
  afterSeconds?: number;
  sampleCount: number;
  maxWidth: number;
  sourceTimesSeconds?: number[];
  exactRepresentationPts?: number[];
  expectedRepresentationFingerprint?: string | null;
  includeAudio?: boolean;
  includeTranscript?: boolean;
}

export interface InspectionFile {
  path: string;
  blake3: string;
  byteLength: number;
}

export interface InspectionTimeMapping {
  timeBaseNumerator: number;
  timeBaseDenominator: number;
  representationOriginPts: number;
  sourceOffsetSeconds: number;
  representationOffsetSeconds: number;
  secondsScale: number;
  precision: string;
  sourceFrameReduction: number | null;
}

export interface InspectedFrame {
  evidenceId: string;
  representationPts: number;
  sourceSeconds: number;
  excerptSeconds: number;
  image: InspectionFile;
}

export interface InspectionWaveformBin {
  excerptStartSeconds: number;
  excerptEndSeconds: number;
  peak: number;
  rms: number;
  lowEnergy: boolean;
}

export interface InspectedAudio {
  state: string;
  playable: InspectionFile | null;
  sampleRate: number;
  channels: number;
  sourceInSamples: number;
  sourceOutSamples: number;
  streamSourceInSeconds: number | null;
  leadingGapSeconds: number;
  waveform: InspectionWaveformBin[];
}

export interface InspectedUtterance {
  sourceInSeconds: number;
  sourceOutSeconds: number;
  excerptInSeconds: number;
  excerptOutSeconds: number;
  text: string;
  clippedByWindow: boolean;
}

export interface SourceInspection {
  evidenceId: string;
  recipe: string;
  request: SourceInspectionRequest;
  sourceFingerprint: string;
  representationFingerprint: string;
  rotationDegrees: number;
  mapping: InspectionTimeMapping;
  windowInSeconds: number;
  windowOutSeconds: number;
  frames: InspectedFrame[];
  audio: InspectedAudio | null;
  utterances: InspectedUtterance[];
  availableFrameCount: number;
  minAvailableFrameIntervalSeconds: number | null;
  maxAvailableFrameIntervalSeconds: number | null;
  maxUnsampledGapSeconds: number;
  limitations: string[];
  createdAt: string;
}

export interface SourceInspectionResult {
  inspection: SourceInspection;
  reused: boolean;
}

export type RangeDecision = "allow" | "exclude";
export type RangeDecisionOwner = "human";
export type RangeMedia = "picture" | "audio" | "both";
export type TrimMedia = "picture" | "audio";

export interface SourceTimeRange {
  sourceInSeconds: number;
  sourceOutSeconds: number;
}

export interface ConfirmSourceRangeRequest {
  assetId: string;
  expectedRevision: number;
  expectedSourceFingerprint: string;
  constraintId?: string | null;
  decision: RangeDecision;
  media: RangeMedia;
  decisionOwner: RangeDecisionOwner;
  sourceInSeconds: number;
  sourceOutSeconds: number;
  rationale: string;
  inspectedEvidenceId?: string | null;
  overridesEvidenceIds?: string[];
}

export interface ConfirmedRangeInspection {
  evidenceId: string;
  representationFingerprint: string;
  recipe: string;
  purpose: InspectionPurpose;
  includedAudio: boolean;
  includedTranscript: boolean;
  rotationDegrees: number;
  mapping: InspectionTimeMapping;
  windowInSeconds: number;
  windowOutSeconds: number;
  sampleCount: number;
  maxUnsampledGapSeconds: number;
}

export interface ConfirmedSourceRange {
  constraintId: string;
  projectId: string;
  assetId: string;
  sourceFingerprint: string;
  decision: RangeDecision;
  media: RangeMedia;
  decisionOwner: RangeDecisionOwner;
  sourceInSeconds: number;
  sourceOutSeconds: number;
  rationale: string;
  inspection: ConfirmedRangeInspection | null;
  overridesEvidenceIds: string[];
  active: boolean;
  lastOperationId: string;
  updatedAtMillis: number;
}

export interface SourceRangeConstraints {
  schemaReady: boolean;
  assetId: string;
  sourceFingerprint: string;
  revision: number;
  items: ConfirmedSourceRange[];
  totalItems: number;
  nextOffset: number | null;
  feasiblePictureRanges: SourceTimeRange[];
  feasibleAudioRanges: SourceTimeRange[];
  pictureConflicts: string[];
  audioConflicts: string[];
}

export interface SourceTrimRequest {
  assetId: string;
  inspectedEvidenceId: string;
  media: TrimMedia;
  sourceInSeconds: number;
  sourceOutSeconds: number;
  preferUtterances?: boolean;
}

export interface SourceTrimSuggestion {
  inspectedEvidenceId: string;
  media: TrimMedia;
  constraintsRevision: number;
  cleanRanges: SourceTimeRange[];
  proposedRanges: SourceTimeRange[];
  conflicts: string[];
  limitations: string[];
  listeningRequired: boolean;
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
  artifactId?: string;
  segmentCounts?: PreviewSegmentCounts;
  outputPath: string;
  durationSeconds: number;
  clipCount: number;
  failures: { position: number; assetId: string; message: string }[];
}

export type PreviewProgress =
  | { type: "started"; artifactId?: string | null; sequence?: number | null; clipCount: number; workerCount: number }
  | {
      type: "clipCompleted";
      segmentCounts?: PreviewSegmentCounts;
      artifactId?: string | null;
      sequence?: number | null;
      position: number;
      elapsedSeconds: number;
      estimateRemainingSeconds: number | null;
    }
  | { type: "clipFailed"; artifactId?: string | null; sequence?: number | null; position: number; message: string }
  | { type: "finished"; artifactId?: string | null; sequence?: number | null; segmentCounts?: PreviewSegmentCounts; outputPath: string; durationSeconds: number }
  | { type: "cancelled"; artifactId?: string | null; sequence?: number | null };

export type PreviewArtifactState = "running" | "complete" | "cancelled" | "failed" | "interrupted";
export interface PreviewSegmentCounts { encoded: number; reused: number; invalidated: number }
export interface CalibrationToolIdentity { role: string; name: string; version: string; executableKey: string }
export interface PreviewSourceIdentity { assetId: string; sourceFingerprint: string; representation: string; representationFingerprint: string; rotationDegrees: number; clocks: unknown }
export interface PreviewInputIdentity { recipe: string; revisionId: string; planDigest: string; maxHeight: number; constraintsKey: string; sources: PreviewSourceIdentity[]; tools: CalibrationToolIdentity[]; toolContentFingerprints?: string[] }
export interface PreviewCutMapping { operationId: string; assetId: string; sourceInSeconds: number; sourceOutSeconds: number; movieInPts: number; movieOutPts: number; outputFrameCount: number }
export interface PreviewMediaValidation { outputFingerprint: string; byteLength: number; durationSeconds: number; videoFrameCount: number; videoTimeBaseNumerator: number; videoTimeBaseDenominator: number; videoOriginPts: number; movieTimeBaseNumerator: number; movieTimeBaseDenominator: number; movieOriginPts: number; width: number; height: number; audioSampleRate: number; audioChannels: number; fullyDecoded: boolean; cuts: PreviewCutMapping[]; limitations: string[] }
export interface PreviewCutOutcome { operationId: string; position: number; assetId: string; state: string; reused: boolean; cacheKey: string | null; failure: string | null }
export interface PreviewArtifact { artifactId: string; projectId: string; revisionId: string; generation: number; request: { revisionId: string; maxHeight: number }; kind: string; state: PreviewArtifactState; sequence: number; stage: string; cancelRequested: boolean; identity: PreviewInputIdentity | null; result: PreviewResult | null; validation: PreviewMediaValidation | null; outputRelativePath: string | null; failure: string | null; startedAt: string; completedAt: string | null; cutOutcomes: PreviewCutOutcome[]; outcomeTotal: number; outcomesTruncated: boolean }
export interface PreviewArtifactVerification { verifiedAt: string; outputMatches: boolean; inputsMatch: boolean; constraintsMatch: boolean; planRenderable: boolean; limitations: string[] }
export interface PreviewArtifactStatus { artifact: PreviewArtifact; observedState: PreviewArtifactState; latestAttempt: boolean; activeRevisionId: string | null; filePresent: boolean; currentForRequest: boolean | null; verification: PreviewArtifactVerification | null }
export interface PreviewArtifactSummary { artifactId: string; revisionId: string; kind: string; generation: number; sequence: number; state: PreviewArtifactState; observedState: PreviewArtifactState; stage: string; cancelRequested: boolean; maxHeight: number; startedAt: string; completedAt: string | null; outputPath: string | null; durationSeconds: number | null; clipCount: number | null }
export interface PreviewArtifactPage { schemaReady: boolean; revisionId: string; total: number; nextOffset: number | null; items: PreviewArtifactSummary[] }

export interface OtioExportResult {
  outputPath: string;
  outputDigest: string;
  clipCount: number;
}
