// The calibration, diagnostics, and edit-plan client (D01-T015, D01-T016,
// D01-T023).
//
// Every function is a thin `invoke`. No chronology rule lives here — the
// desktop must show what the solver decided, not its own opinion of it. That
// holds in the browser preview too: it installs the shared fake backend
// (`src/preview/`), which serves a *declared* chronology rather than one it
// worked out. Neither the preview nor the CUJ harness is evidence for
// persistence, media, or native behaviour.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  CaptureSequence,
  CaptureSequenceMergePreview,
  CaptureSequenceReassignmentPreview,
  CaptureSequenceSplitPreview,
  ChronologyExplanation,
  ChronologyImpact,
  ChronologyOverview,
  ClipTimeException,
  ClockEpoch,
  ClockEpochClearPreview,
  ClockEpochImpactPreview,
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
  VisualPlacementPreview,
} from "./chronologyTypes";

const inTauri = () => "__TAURI_INTERNALS__" in window;

export const chronologyApi = {
  // --- read (D01-T015) ------------------------------------------------------

  async overview(projectId: string): Promise<ChronologyOverview> {
    return invoke<ChronologyOverview>("chronology_overview", { projectId });
  },

  async explain(assetId: string): Promise<ChronologyExplanation | null> {
    return invoke<ChronologyExplanation>("explain_chronology", { assetId });
  },

  // --- calibration editing (D01-T016) ---------------------------------------

  /**
   * What a candidate epoch *would* do, computed without writing anything.
   * No chronology change is applied without explicit user confirmation, so
   * this always runs before the corresponding write.
   */
  async previewClockEpoch(projectId: string, epoch: ClockEpoch): Promise<Contradiction[]> {
    return invoke<Contradiction[]>("preview_clock_epoch", { projectId, epoch });
  },

  async previewClockEpochImpact(
    projectId: string,
    epoch: ClockEpoch,
  ): Promise<ClockEpochImpactPreview> {
    return invoke<ClockEpochImpactPreview>("preview_clock_epoch_impact", { projectId, epoch });
  },

  async previewClearClockEpochImpact(
    projectId: string,
    epochId: string,
  ): Promise<ClockEpochClearPreview> {
    return invoke<ClockEpochClearPreview>("preview_clear_clock_epoch_impact", { projectId, epochId });
  },

  async previewVisualPlacement(
    projectId: string,
    assetIds: string[],
    targetAssetId: string,
    side: "before" | "after",
    label: string,
  ): Promise<VisualPlacementPreview> {
    return invoke<VisualPlacementPreview>("preview_visual_placement", {
      projectId,
      assetIds,
      targetAssetId,
      side,
      label,
    });
  },

  async setClockEpoch(
    projectId: string,
    preview: ClockEpochImpactPreview | VisualPlacementPreview,
  ): Promise<ClockEpoch | null> {
    return invoke<ClockEpoch>("set_clock_epoch", { projectId, preview });
  },

  async clearClockEpoch(projectId: string, preview: ClockEpochClearPreview): Promise<void> {
    await invoke("clear_clock_epoch", { projectId, preview });
  },

  async splitCaptureSequence(
    projectId: string,
    sourceSequenceId: string,
    atCounter: number,
    label: string,
  ): Promise<CaptureSequence | null> {
    return invoke<CaptureSequence>("split_capture_sequence", {
      projectId,
      sourceSequenceId,
      atCounter,
      label,
    });
  },

  /**
   * Records "no, that counter restart is not a card change".
   *
   * No impact preview, unlike every other chronology write on this surface:
   * the clips were already in one sequence and stay there, so there is no
   * placement change to show. `restoreCaptureSequenceSplit` reverses it.
   */
  async declineCaptureSequenceSplit(
    projectId: string,
    captureSequenceId: string,
    atCounter: number,
    reason: string,
  ): Promise<void> {
    await invoke("decline_capture_sequence_split", {
      projectId,
      captureSequenceId,
      atCounter,
      reason,
    });
  },

  async restoreCaptureSequenceSplit(
    projectId: string,
    captureSequenceId: string,
    atCounter: number,
  ): Promise<void> {
    await invoke("restore_capture_sequence_split", { projectId, captureSequenceId, atCounter });
  },

  async previewCaptureSequenceSplit(
    projectId: string,
    sourceSequenceId: string,
    atCounter: number,
    label: string,
    assetIds: string[],
  ): Promise<CaptureSequenceSplitPreview> {
    return invoke<CaptureSequenceSplitPreview>("preview_capture_sequence_split", {
      projectId,
      sourceSequenceId,
      atCounter,
      label,
      assetIds,
    });
  },

  async applyCaptureSequenceSplit(
    projectId: string,
    preview: CaptureSequenceSplitPreview,
  ): Promise<CaptureSequence> {
    return invoke<CaptureSequence>("apply_capture_sequence_split", { projectId, preview });
  },

  async mergeCaptureSequences(
    projectId: string,
    fromSequenceId: string,
    intoSequenceId: string,
  ): Promise<void> {
    await invoke("merge_capture_sequences", { projectId, fromSequenceId, intoSequenceId });
  },

  async previewCaptureSequenceMerge(
    projectId: string,
    fromSequenceId: string,
    intoSequenceId: string,
  ): Promise<CaptureSequenceMergePreview> {
    return invoke<CaptureSequenceMergePreview>("preview_capture_sequence_merge", {
      projectId,
      fromSequenceId,
      intoSequenceId,
    });
  },

  async applyCaptureSequenceMerge(
    projectId: string,
    preview: CaptureSequenceMergePreview,
  ): Promise<void> {
    await invoke("apply_capture_sequence_merge", { projectId, preview });
  },

  async previewCaptureSequenceReassignment(
    projectId: string,
    anchorAssetId: string,
    intoSequenceId: string,
  ): Promise<CaptureSequenceReassignmentPreview> {
    return invoke<CaptureSequenceReassignmentPreview>("preview_capture_sequence_reassignment",
      { projectId, anchorAssetId, intoSequenceId },
    );
  },

  async previewCaptureSequenceAssets(
    projectId: string,
    fromSequenceId: string,
    intoSequenceId: string,
    assetIds: string[],
  ): Promise<CaptureSequenceReassignmentPreview> {
    return invoke<CaptureSequenceReassignmentPreview>("preview_capture_sequence_assets", {
      projectId,
      fromSequenceId,
      intoSequenceId,
      assetIds,
    });
  },

  async reassignCaptureSequenceAssets(
    projectId: string,
    preview: CaptureSequenceReassignmentPreview,
  ): Promise<string[]> {
    return invoke<string[]>("reassign_capture_sequence_assets", { projectId, preview });
  },

  async setClipTimeException(
    projectId: string,
    assetId: string,
    tripTime: string,
    reason: string,
  ): Promise<ClipTimeException | null> {
    return invoke<ClipTimeException>("set_clip_time_exception", {
      projectId,
      assetId,
      tripTime,
      reason,
    });
  },

  async clearClipTimeException(projectId: string, assetId: string): Promise<void> {
    await invoke("clear_clip_time_exception", { projectId, assetId });
  },

  async setTripPhase(projectId: string, phase: TripPhase): Promise<TripPhase | null> {
    return invoke<TripPhase>("set_trip_phase", { projectId, phase });
  },

  async splitTripPhase(
    projectId: string,
    existingPhaseId: string,
    boundaryAssetId: string,
    direction: PhaseSplitDirection,
    newPhase: TripPhase,
  ): Promise<TripPhase | null> {
    return invoke<TripPhase>("split_trip_phase", {
      projectId,
      existingPhaseId,
      boundaryAssetId,
      direction,
      newPhase,
    });
  },

  async removeTripPhase(projectId: string, phaseId: string): Promise<void> {
    await invoke("remove_trip_phase", { projectId, phaseId });
  },

  async setEventOrderConstraint(
    projectId: string,
    earlierAssetId: string,
    laterAssetId: string,
    strength: "hard" | "soft",
    source: string,
  ): Promise<EventOrderConstraint | null> {
    return invoke<EventOrderConstraint>("set_event_order_constraint", {
      projectId,
      earlierAssetId,
      laterAssetId,
      strength,
      source,
    });
  },

  async clearEventOrderConstraint(projectId: string, constraintId: string): Promise<void> {
    await invoke("clear_event_order_constraint", { projectId, constraintId });
  },

  /** Call before applying a change; capturing afterwards captures the damage. */
  async captureQueuePosition(
    projectId: string,
    currentAssetId: string,
  ): Promise<QueuePosition | null> {
    return invoke<QueuePosition>("capture_queue_position", { projectId, currentAssetId });
  },

  /** Call after applying a change. Returns the asset to resume on. */
  async restoreQueuePosition(
    projectId: string,
    position: QueuePosition,
  ): Promise<string | null> {
    return invoke<string>("restore_queue_position", { projectId, position });
  },

  // --- diagnostics (D01-T027 consumer) --------------------------------------

  async diagnostics(
    projectId: string,
    options?: { severity?: string; kindPrefix?: string; includeAcknowledged?: boolean },
  ): Promise<DiagnosticsView> {
    return invoke<DiagnosticsView>("list_diagnostics", {
      projectId,
      severity: options?.severity ?? null,
      kindPrefix: options?.kindPrefix ?? null,
      includeAcknowledged: options?.includeAcknowledged ?? false,
    });
  },

  async acknowledgeDiagnostic(projectId: string, diagnosticId: string): Promise<void> {
    await invoke("acknowledge_diagnostic", { projectId, diagnosticId });
  },

  /** Run on project open so an unmounted card is a visible condition. */
  async detectOfflineVolumes(projectId: string): Promise<Diagnostic[]> {
    return invoke<Diagnostic[]>("detect_offline_volumes", { projectId });
  },

  // --- edit plan and export (D01-T023) --------------------------------------

  async activeEditPlan(projectId: string): Promise<EditPlanRevision | null> {
    return invoke<EditPlanRevision | null>("active_edit_plan", { projectId });
  },

  async editPlanRevisions(projectId: string): Promise<EditPlanRevision[]> {
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
    return invoke<EditPlanRevision>("activate_edit_plan_revision", {
      projectId,
      revisionId,
      expectedActiveRevisionId,
    });
  },

  async validateEditPlan(revisionId: string): Promise<ValidationReport> {
    return invoke<ValidationReport>("validate_edit_plan", { revisionId });
  },

  async renderPreview(revisionId: string, maxHeight?: number): Promise<PreviewResult | null> {
    return invoke<PreviewResult>("render_edit_plan_preview", {
      revisionId,
      maxHeight: maxHeight ?? 540,
    });
  },

  async onPreviewProgress(
    callback: (progress: PreviewProgress) => void,
  ): Promise<() => void> {
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
