// The calibration, diagnostics, and edit-plan client (D01-T015, D01-T016,
// D01-T023).
//
// Every function is a thin `invoke`. No chronology rule lives here — the
// desktop must show what the solver decided, not its own opinion of it.
//
// Outside Tauri the browser mock returns empty results rather than plausible
// fake data. The Vite browser mock is never evidence for persistence, media, or
// native behaviour, and a mock that invents a chronology would let someone
// believe otherwise.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  CaptureSequence,
  ChronologyExplanation,
  ChronologyOverview,
  ClipTimeException,
  ClockEpoch,
  Contradiction,
  Diagnostic,
  DiagnosticsView,
  EditPlanRevision,
  EventOrderConstraint,
  OtioExportResult,
  PreviewProgress,
  PreviewResult,
  PhaseSplitDirection,
  QueuePosition,
  TripPhase,
  ValidationReport,
} from "./chronologyTypes";

const inTauri = () => "__TAURI_INTERNALS__" in window;

const emptyOverview: ChronologyOverview = {
  sequences: [],
  epochs: [],
  exceptions: [],
  phases: [],
  constraints: [],
  contradictions: [],
  assets: [],
  proposedSplits: [],
  unplacedAssets: 0,
};

export const chronologyApi = {
  // --- read (D01-T015) ------------------------------------------------------

  async overview(projectId: string): Promise<ChronologyOverview> {
    if (!inTauri()) return emptyOverview;
    return invoke<ChronologyOverview>("chronology_overview", { projectId });
  },

  async explain(assetId: string): Promise<ChronologyExplanation | null> {
    if (!inTauri()) return null;
    return invoke<ChronologyExplanation>("explain_chronology", { assetId });
  },

  // --- calibration editing (D01-T016) ---------------------------------------

  /**
   * What a candidate epoch *would* do, computed without writing anything.
   * No chronology change is applied without explicit user confirmation, so
   * this always runs before the corresponding write.
   */
  async previewClockEpoch(projectId: string, epoch: ClockEpoch): Promise<Contradiction[]> {
    if (!inTauri()) return [];
    return invoke<Contradiction[]>("preview_clock_epoch", { projectId, epoch });
  },

  async setClockEpoch(projectId: string, epoch: ClockEpoch): Promise<ClockEpoch | null> {
    if (!inTauri()) return null;
    return invoke<ClockEpoch>("set_clock_epoch", { projectId, epoch });
  },

  async clearClockEpoch(projectId: string, epochId: string): Promise<void> {
    if (!inTauri()) return;
    await invoke("clear_clock_epoch", { projectId, epochId });
  },

  async splitCaptureSequence(
    projectId: string,
    sourceSequenceId: string,
    atCounter: number,
    label: string,
  ): Promise<CaptureSequence | null> {
    if (!inTauri()) return null;
    return invoke<CaptureSequence>("split_capture_sequence", {
      projectId,
      sourceSequenceId,
      atCounter,
      label,
    });
  },

  async mergeCaptureSequences(
    projectId: string,
    fromSequenceId: string,
    intoSequenceId: string,
  ): Promise<void> {
    if (!inTauri()) return;
    await invoke("merge_capture_sequences", { projectId, fromSequenceId, intoSequenceId });
  },

  async setClipTimeException(
    projectId: string,
    assetId: string,
    tripTime: string,
    reason: string,
  ): Promise<ClipTimeException | null> {
    if (!inTauri()) return null;
    return invoke<ClipTimeException>("set_clip_time_exception", {
      projectId,
      assetId,
      tripTime,
      reason,
    });
  },

  async clearClipTimeException(projectId: string, assetId: string): Promise<void> {
    if (!inTauri()) return;
    await invoke("clear_clip_time_exception", { projectId, assetId });
  },

  async setTripPhase(projectId: string, phase: TripPhase): Promise<TripPhase | null> {
    if (!inTauri()) return null;
    return invoke<TripPhase>("set_trip_phase", { projectId, phase });
  },

  async splitTripPhase(
    projectId: string,
    existingPhaseId: string,
    boundaryAssetId: string,
    direction: PhaseSplitDirection,
    newPhase: TripPhase,
  ): Promise<TripPhase | null> {
    if (!inTauri()) return null;
    return invoke<TripPhase>("split_trip_phase", {
      projectId,
      existingPhaseId,
      boundaryAssetId,
      direction,
      newPhase,
    });
  },

  async removeTripPhase(projectId: string, phaseId: string): Promise<void> {
    if (!inTauri()) return;
    await invoke("remove_trip_phase", { projectId, phaseId });
  },

  async setEventOrderConstraint(
    projectId: string,
    earlierAssetId: string,
    laterAssetId: string,
    strength: "hard" | "soft",
    source: string,
  ): Promise<EventOrderConstraint | null> {
    if (!inTauri()) return null;
    return invoke<EventOrderConstraint>("set_event_order_constraint", {
      projectId,
      earlierAssetId,
      laterAssetId,
      strength,
      source,
    });
  },

  async clearEventOrderConstraint(projectId: string, constraintId: string): Promise<void> {
    if (!inTauri()) return;
    await invoke("clear_event_order_constraint", { projectId, constraintId });
  },

  /** Call before applying a change; capturing afterwards captures the damage. */
  async captureQueuePosition(
    projectId: string,
    currentAssetId: string,
  ): Promise<QueuePosition | null> {
    if (!inTauri()) return null;
    return invoke<QueuePosition>("capture_queue_position", { projectId, currentAssetId });
  },

  /** Call after applying a change. Returns the asset to resume on. */
  async restoreQueuePosition(
    projectId: string,
    position: QueuePosition,
  ): Promise<string | null> {
    if (!inTauri()) return null;
    return invoke<string>("restore_queue_position", { projectId, position });
  },

  // --- diagnostics (D01-T027 consumer) --------------------------------------

  async diagnostics(
    projectId: string,
    options?: { severity?: string; kindPrefix?: string; includeAcknowledged?: boolean },
  ): Promise<DiagnosticsView> {
    if (!inTauri()) return { diagnostics: [], counts: { errors: 0, warnings: 0, infos: 0 } };
    return invoke<DiagnosticsView>("list_diagnostics", {
      projectId,
      severity: options?.severity ?? null,
      kindPrefix: options?.kindPrefix ?? null,
      includeAcknowledged: options?.includeAcknowledged ?? false,
    });
  },

  async acknowledgeDiagnostic(projectId: string, diagnosticId: string): Promise<void> {
    if (!inTauri()) return;
    await invoke("acknowledge_diagnostic", { projectId, diagnosticId });
  },

  /** Run on project open so an unmounted card is a visible condition. */
  async detectOfflineVolumes(projectId: string): Promise<Diagnostic[]> {
    if (!inTauri()) return [];
    return invoke<Diagnostic[]>("detect_offline_volumes", { projectId });
  },

  // --- edit plan and export (D01-T023) --------------------------------------

  async activeEditPlan(projectId: string): Promise<EditPlanRevision | null> {
    if (!inTauri()) return null;
    return invoke<EditPlanRevision | null>("active_edit_plan", { projectId });
  },

  async editPlanRevisions(projectId: string): Promise<EditPlanRevision[]> {
    if (!inTauri()) return [];
    return invoke<EditPlanRevision[]>("edit_plan_revisions", { projectId });
  },

  async createEditPlanRevision(
    projectId: string,
    operations: {
      assetId: string;
      sourceInSeconds: number;
      sourceOutSeconds: number;
      rationale: string;
    }[],
    expectedActiveRevisionId: string | null,
  ): Promise<EditPlanRevision | null> {
    if (!inTauri()) return null;
    return invoke<EditPlanRevision>("create_edit_plan_revision", {
      projectId,
      operations,
      scriptRevisionId: null,
      expectedActiveRevisionId,
    });
  },

  async activateEditPlanRevision(
    projectId: string,
    revisionId: string,
    expectedActiveRevisionId: string | null,
  ): Promise<EditPlanRevision | null> {
    if (!inTauri()) return null;
    return invoke<EditPlanRevision>("activate_edit_plan_revision", {
      projectId,
      revisionId,
      expectedActiveRevisionId,
    });
  },

  async validateEditPlan(revisionId: string): Promise<ValidationReport> {
    if (!inTauri()) return { findings: [] };
    return invoke<ValidationReport>("validate_edit_plan", { revisionId });
  },

  async renderPreview(revisionId: string, maxHeight?: number): Promise<PreviewResult | null> {
    if (!inTauri()) return null;
    return invoke<PreviewResult>("render_edit_plan_preview", {
      revisionId,
      maxHeight: maxHeight ?? 540,
    });
  },

  async onPreviewProgress(
    callback: (progress: PreviewProgress) => void,
  ): Promise<() => void> {
    if (!inTauri()) return () => {};
    return listen<PreviewProgress>("edit-plan-preview-progress", (event) =>
      callback(event.payload),
    );
  },

  async exportOtio(
    projectId: string,
    revisionId: string,
    outputPath: string,
    timelineName: string,
    expectedExistingDigest: string | null,
  ): Promise<OtioExportResult | null> {
    if (!inTauri()) return null;
    return invoke<OtioExportResult>("export_edit_plan_otio", {
      projectId,
      revisionId,
      outputPath,
      timelineName,
      expectedExistingDigest,
    });
  },

  isDesktop: inTauri,
};
