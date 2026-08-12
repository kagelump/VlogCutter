//! Tauri commands for the calibration surface (D01-T015, D01-T016), the
//! diagnostics surface (D01-T027 consumer), and the edit-plan and export
//! surface (D01-T023).
//!
//! Every command here is a thin adapter: it moves arguments into
//! `rcdvlog-core` and moves results back. No chronology rule, no ordering
//! decision, and no validation lives in this file — the desktop and the CLI
//! must never be able to disagree, and the only way to guarantee that is for
//! neither of them to own any logic.
//!
//! Writes wake the sync manager, exactly as the pre-existing mutating commands
//! do.

use crate::{AppState, error_message};
use rcdvlog_core::{
    AssetChronology, CaptureSequence, CaptureSequenceMergePreview,
    CaptureSequenceReassignmentPreview, CaptureSequenceSplitPreview, ChronologyExplanation,
    ChronologySolution, ClipTimeException, ClockEpoch, ClockEpochClearPreview,
    ClockEpochImpactPreview, ConstraintStrength, Contradiction, Diagnostic, DiagnosticCounts,
    DiagnosticFilter, DiagnosticSeverity, DisplayMode, EditPlanOperation, EditPlanRevision,
    EventOrderConstraint, OtioExportRequest, OtioExportResult, PhaseKind, PhaseSplitDirection,
    PreviewProgress, PreviewRequest, PreviewResult, QueuePosition, TripPhase, ValidationReport,
    VisualPlacementPreview, VisualPlacementSide, resolve_anchored_phases,
};
use serde::{Deserialize, Serialize};
use tauri::State;

/// Everything the calibration surface reads in one round trip.
///
/// One call rather than six, because six calls can interleave with a write and
/// show the user a view that never existed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChronologyOverview {
    pub sequences: Vec<CaptureSequence>,
    pub epochs: Vec<ClockEpoch>,
    pub exceptions: Vec<ClipTimeException>,
    pub phases: Vec<TripPhase>,
    pub constraints: Vec<EventOrderConstraint>,
    pub contradictions: Vec<Contradiction>,
    pub assets: Vec<AssetChronology>,
    pub proposed_splits: Vec<ProposedSplit>,
    pub declined_splits: Vec<DeclinedSplit>,
    pub unplaced_assets: u32,
}

/// A counter restart the user answered "same card" to. Carried on the overview
/// so the desk can list those answers and take them back.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclinedSplit {
    pub capture_sequence_id: String,
    pub at_counter: u64,
    pub reason: String,
    pub declined_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposedSplit {
    pub capture_sequence_id: String,
    pub at_counter: u64,
    pub reason: String,
    pub affected_asset_ids: Vec<String>,
}

/// A trip phase as the UI sends it, with the enums as plain strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TripPhaseInput {
    pub id: String,
    pub label: String,
    pub kind: String,
    #[serde(default)]
    pub start_anchor_asset_id: Option<String>,
    #[serde(default)]
    pub end_anchor_asset_id: Option<String>,
    pub start_trip_time: String,
    pub end_trip_time: String,
    pub timezone: Option<String>,
    pub origin_timezone: Option<String>,
    pub destination_timezone: Option<String>,
    pub display_mode: String,
}

impl TripPhaseInput {
    fn into_phase(self) -> Result<TripPhase, String> {
        let kind = match self.kind.as_str() {
            "location" => PhaseKind::Location,
            "transit" => PhaseKind::Transit,
            other => return Err(format!("unknown trip phase kind: {other}")),
        };
        let display_mode = match self.display_mode.as_str() {
            "local" => DisplayMode::Local,
            "dual" => DisplayMode::Dual,
            "utc" => DisplayMode::Utc,
            other => return Err(format!("unknown trip phase display mode: {other}")),
        };
        Ok(TripPhase {
            id: self.id,
            label: self.label,
            kind,
            start_anchor_asset_id: self.start_anchor_asset_id,
            end_anchor_asset_id: self.end_anchor_asset_id,
            start_trip_time: self.start_trip_time,
            end_trip_time: self.end_trip_time,
            timezone: self.timezone,
            origin_timezone: self.origin_timezone,
            destination_timezone: self.destination_timezone,
            display_mode,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockEpochInput {
    pub id: String,
    pub capture_sequence_id: String,
    pub start_counter: u64,
    pub end_counter: Option<u64>,
    pub offset_seconds: i64,
    pub confidence: f32,
    pub evidence: Vec<String>,
    pub label: String,
}

impl ClockEpochInput {
    fn into_epoch(self) -> ClockEpoch {
        ClockEpoch {
            id: self.id,
            capture_sequence_id: self.capture_sequence_id,
            start_counter: self.start_counter,
            end_counter: self.end_counter,
            offset_seconds: self.offset_seconds,
            confidence: self.confidence,
            evidence: self.evidence,
            label: self.label,
        }
    }
}

fn strength_from(value: &str) -> Result<ConstraintStrength, String> {
    match value {
        "hard" => Ok(ConstraintStrength::Hard),
        "soft" => Ok(ConstraintStrength::Soft),
        other => Err(format!("unknown event-order constraint strength: {other}")),
    }
}

#[cfg(test)]
mod transport_input_tests {
    use super::*;

    fn phase_input(kind: &str, display_mode: &str) -> TripPhaseInput {
        TripPhaseInput {
            id: "phase_1".to_string(),
            label: "Flight".to_string(),
            kind: kind.to_string(),
            start_anchor_asset_id: Some("asset_a".to_string()),
            end_anchor_asset_id: Some("asset_b".to_string()),
            start_trip_time: "2026-02-08T00:00:00Z".to_string(),
            end_trip_time: "2026-02-08T01:00:00Z".to_string(),
            timezone: None,
            origin_timezone: Some("Asia/Tokyo".to_string()),
            destination_timezone: Some("Europe/Madrid".to_string()),
            display_mode: display_mode.to_string(),
        }
    }

    #[test]
    fn chronology_transport_rejects_unknown_enums_instead_of_coercing_them() {
        assert!(phase_input("journey", "local").into_phase().is_err());
        assert!(phase_input("transit", "guess").into_phase().is_err());
        assert!(strength_from("advisory").is_err());
    }

    #[test]
    fn chronology_transport_accepts_each_declared_enum_value() {
        assert!(matches!(
            phase_input("location", "local").into_phase().unwrap().kind,
            PhaseKind::Location
        ));
        let transit = phase_input("transit", "dual").into_phase().unwrap();
        assert!(matches!(transit.kind, PhaseKind::Transit));
        assert!(matches!(transit.display_mode, DisplayMode::Dual));
        assert!(matches!(
            strength_from("soft").unwrap(),
            ConstraintStrength::Soft
        ));
        assert!(matches!(
            strength_from("hard").unwrap(),
            ConstraintStrength::Hard
        ));
    }
}

fn refresh_asset_projection(
    catalog: &rcdvlog_core::Catalog,
    project_id: &str,
) -> Result<(), String> {
    catalog
        .resolve_chronology(project_id)
        .map(|_| ())
        .map_err(error_message)
}

// --- D01-T015: read-only calibration surface --------------------------------

#[tauri::command]
pub fn chronology_overview(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<ChronologyOverview, String> {
    let catalog = state.catalog()?;
    let snapshot = catalog
        .chronology_snapshot(&project_id)
        .map_err(error_message)?;
    let facts = snapshot.facts;
    let solution = snapshot.solution;
    let proposals = snapshot.proposed_splits;
    let declined = snapshot.declined_splits;
    let unplaced = solution
        .assets
        .iter()
        .filter(|entry| entry.trip_time.is_none())
        .count() as u32;
    let (phases, _) = resolve_anchored_phases(&facts.phases, &solution.assets);
    Ok(ChronologyOverview {
        sequences: facts.sequences,
        epochs: facts.epochs,
        exceptions: facts.exceptions,
        phases,
        constraints: facts.constraints,
        contradictions: solution.contradictions,
        assets: solution.assets,
        proposed_splits: proposals
            .into_iter()
            .map(|proposal| ProposedSplit {
                capture_sequence_id: proposal.capture_sequence_id,
                at_counter: proposal.at_counter,
                reason: proposal.reason,
                affected_asset_ids: proposal.affected_asset_ids,
            })
            .collect(),
        declined_splits: declined
            .into_iter()
            .map(|entry| DeclinedSplit {
                capture_sequence_id: entry.capture_sequence_id,
                at_counter: entry.at_counter,
                reason: entry.reason,
                declined_at: entry.declined_at,
            })
            .collect(),
        unplaced_assets: unplaced,
    })
}

#[tauri::command]
pub fn chronology_solution(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<ChronologySolution, String> {
    state
        .catalog()?
        .solve_chronology(&project_id)
        .map_err(error_message)
}

#[tauri::command]
pub fn explain_chronology(
    asset_id: String,
    state: State<'_, AppState>,
) -> Result<ChronologyExplanation, String> {
    state
        .catalog()?
        .explain_chronology(&asset_id)
        .map_err(error_message)
}

// --- D01-T016: calibration editing and impact preview -----------------------

/// The impact a candidate epoch *would* have, computed without writing.
///
/// No chronology change is applied without explicit user confirmation, so the
/// user sees the consequence first and confirms second.
#[tauri::command]
pub fn preview_clock_epoch(
    project_id: String,
    epoch: ClockEpochInput,
    state: State<'_, AppState>,
) -> Result<Vec<Contradiction>, String> {
    state
        .catalog()?
        .preview_clock_epoch(&project_id, &epoch.into_epoch())
        .map_err(error_message)
}

#[tauri::command]
pub fn preview_clock_epoch_impact(
    project_id: String,
    epoch: ClockEpochInput,
    state: State<'_, AppState>,
) -> Result<ClockEpochImpactPreview, String> {
    state
        .catalog()?
        .preview_clock_epoch_impact(&project_id, &epoch.into_epoch())
        .map_err(error_message)
}

#[tauri::command]
pub fn preview_clear_clock_epoch_impact(
    project_id: String,
    epoch_id: String,
    state: State<'_, AppState>,
) -> Result<ClockEpochClearPreview, String> {
    state
        .catalog()?
        .preview_clear_clock_epoch_impact(&project_id, &epoch_id)
        .map_err(error_message)
}

#[tauri::command]
pub fn preview_visual_placement(
    project_id: String,
    asset_ids: Vec<String>,
    target_asset_id: String,
    side: String,
    label: String,
    state: State<'_, AppState>,
) -> Result<VisualPlacementPreview, String> {
    let side = match side.as_str() {
        "before" => VisualPlacementSide::Before,
        "after" => VisualPlacementSide::After,
        _ => return Err("placement side must be before or after".to_string()),
    };
    state
        .catalog()?
        .preview_visual_placement(&project_id, &asset_ids, &target_asset_id, side, &label)
        .map_err(error_message)
}

#[tauri::command]
pub fn set_clock_epoch(
    project_id: String,
    preview: ClockEpochImpactPreview,
    state: State<'_, AppState>,
) -> Result<ClockEpoch, String> {
    let catalog = state.catalog()?;
    let stored = catalog
        .apply_clock_epoch_preview(&project_id, &preview)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    Ok(stored)
}

#[tauri::command]
pub fn clear_clock_epoch(
    project_id: String,
    preview: ClockEpochClearPreview,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let catalog = state.catalog()?;
    catalog
        .apply_clear_clock_epoch_preview(&project_id, &preview)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    Ok(())
}

#[tauri::command]
pub fn preview_capture_sequence_split(
    project_id: String,
    source_sequence_id: String,
    at_counter: u64,
    label: String,
    asset_ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<CaptureSequenceSplitPreview, String> {
    state
        .catalog()?
        .preview_capture_sequence_split(
            &project_id,
            &source_sequence_id,
            at_counter,
            &label,
            &asset_ids,
        )
        .map_err(error_message)
}

#[tauri::command]
pub fn apply_capture_sequence_split(
    project_id: String,
    preview: CaptureSequenceSplitPreview,
    state: State<'_, AppState>,
) -> Result<CaptureSequence, String> {
    let catalog = state.catalog()?;
    let created = catalog
        .apply_capture_sequence_split(&project_id, &preview)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    Ok(created)
}

/// "No, that counter restart is not a card change."
///
/// Unlike a split this changes no placement, so it needs no impact preview:
/// there is nothing to preview. It is reversible through
/// `restore_capture_sequence_split`.
#[tauri::command]
pub fn decline_capture_sequence_split(
    project_id: String,
    capture_sequence_id: String,
    at_counter: u64,
    reason: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let catalog = state.catalog()?;
    catalog
        .decline_capture_sequence_split(&project_id, &capture_sequence_id, at_counter, &reason)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    Ok(())
}

#[tauri::command]
pub fn restore_capture_sequence_split(
    project_id: String,
    capture_sequence_id: String,
    at_counter: u64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let catalog = state.catalog()?;
    catalog
        .restore_capture_sequence_split(&project_id, &capture_sequence_id, at_counter)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    Ok(())
}

#[tauri::command]
pub fn split_capture_sequence(
    project_id: String,
    source_sequence_id: String,
    at_counter: u64,
    label: String,
    state: State<'_, AppState>,
) -> Result<CaptureSequence, String> {
    let catalog = state.catalog()?;
    let created = catalog
        .split_capture_sequence(&project_id, &source_sequence_id, at_counter, &label)
        .map_err(error_message)?;
    refresh_asset_projection(&catalog, &project_id)?;
    state.sync_manager.wake_project(&project_id);
    Ok(created)
}

#[tauri::command]
pub fn merge_capture_sequences(
    project_id: String,
    from_sequence_id: String,
    into_sequence_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let catalog = state.catalog()?;
    catalog
        .merge_capture_sequences(&project_id, &from_sequence_id, &into_sequence_id)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    Ok(())
}

#[tauri::command]
pub fn preview_capture_sequence_merge(
    project_id: String,
    from_sequence_id: String,
    into_sequence_id: String,
    state: State<'_, AppState>,
) -> Result<CaptureSequenceMergePreview, String> {
    state
        .catalog()?
        .preview_capture_sequence_merge(&project_id, &from_sequence_id, &into_sequence_id)
        .map_err(error_message)
}

#[tauri::command]
pub fn apply_capture_sequence_merge(
    project_id: String,
    preview: CaptureSequenceMergePreview,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let catalog = state.catalog()?;
    catalog
        .apply_capture_sequence_merge(&project_id, &preview)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    Ok(())
}

#[tauri::command]
pub fn preview_capture_sequence_reassignment(
    project_id: String,
    anchor_asset_id: String,
    into_sequence_id: String,
    state: State<'_, AppState>,
) -> Result<CaptureSequenceReassignmentPreview, String> {
    state
        .catalog()?
        .preview_capture_sequence_reassignment(&project_id, &anchor_asset_id, &into_sequence_id)
        .map_err(error_message)
}

#[tauri::command]
pub fn preview_capture_sequence_assets(
    project_id: String,
    from_sequence_id: String,
    into_sequence_id: String,
    asset_ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<CaptureSequenceReassignmentPreview, String> {
    state
        .catalog()?
        .preview_capture_sequence_assets(
            &project_id,
            &from_sequence_id,
            &into_sequence_id,
            &asset_ids,
        )
        .map_err(error_message)
}

#[tauri::command]
pub fn reassign_capture_sequence_assets(
    project_id: String,
    preview: CaptureSequenceReassignmentPreview,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let catalog = state.catalog()?;
    let moved = catalog
        .apply_capture_sequence_reassignment(&project_id, &preview)
        .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    Ok(moved)
}

#[tauri::command]
pub fn set_clip_time_exception(
    project_id: String,
    asset_id: String,
    trip_time: String,
    reason: String,
    state: State<'_, AppState>,
) -> Result<ClipTimeException, String> {
    let catalog = state.catalog()?;
    let stored = catalog
        .set_clip_time_exception(&asset_id, &trip_time, &reason)
        .map_err(error_message)?;
    refresh_asset_projection(&catalog, &project_id)?;
    state.sync_manager.wake_project(&project_id);
    Ok(stored)
}

#[tauri::command]
pub fn clear_clip_time_exception(
    project_id: String,
    asset_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let catalog = state.catalog()?;
    catalog
        .clear_clip_time_exception(&asset_id)
        .map_err(error_message)?;
    refresh_asset_projection(&catalog, &project_id)?;
    state.sync_manager.wake_project(&project_id);
    Ok(())
}

#[tauri::command]
pub fn set_trip_phase(
    project_id: String,
    phase: TripPhaseInput,
    state: State<'_, AppState>,
) -> Result<TripPhase, String> {
    let phase = phase.into_phase()?;
    let catalog = state.catalog()?;
    let stored = catalog
        .set_trip_phase(&project_id, &phase)
        .map_err(error_message)?;
    refresh_asset_projection(&catalog, &project_id)?;
    state.sync_manager.wake_project(&project_id);
    Ok(stored)
}

#[tauri::command]
pub fn split_trip_phase(
    project_id: String,
    existing_phase_id: String,
    boundary_asset_id: String,
    direction: PhaseSplitDirection,
    new_phase: TripPhaseInput,
    state: State<'_, AppState>,
) -> Result<TripPhase, String> {
    let new_phase = new_phase.into_phase()?;
    let catalog = state.catalog()?;
    let stored = catalog
        .split_trip_phase(
            &project_id,
            &existing_phase_id,
            &boundary_asset_id,
            direction,
            &new_phase,
        )
        .map_err(error_message)?;
    refresh_asset_projection(&catalog, &project_id)?;
    state.sync_manager.wake_project(&project_id);
    Ok(stored)
}

#[tauri::command]
pub fn remove_trip_phase(
    project_id: String,
    phase_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let catalog = state.catalog()?;
    catalog
        .remove_trip_phase(&project_id, &phase_id)
        .map_err(error_message)?;
    refresh_asset_projection(&catalog, &project_id)?;
    state.sync_manager.wake_project(&project_id);
    Ok(())
}

#[tauri::command]
pub fn set_event_order_constraint(
    project_id: String,
    earlier_asset_id: String,
    later_asset_id: String,
    strength: String,
    source: String,
    state: State<'_, AppState>,
) -> Result<EventOrderConstraint, String> {
    let strength = strength_from(&strength)?;
    let catalog = state.catalog()?;
    let stored = catalog
        .set_event_order_constraint(
            &project_id,
            &earlier_asset_id,
            &later_asset_id,
            strength,
            &source,
        )
        .map_err(error_message)?;
    refresh_asset_projection(&catalog, &project_id)?;
    state.sync_manager.wake_project(&project_id);
    Ok(stored)
}

#[tauri::command]
pub fn clear_event_order_constraint(
    project_id: String,
    constraint_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let catalog = state.catalog()?;
    catalog
        .clear_event_order_constraint(&project_id, &constraint_id)
        .map_err(error_message)?;
    refresh_asset_projection(&catalog, &project_id)?;
    state.sync_manager.wake_project(&project_id);
    Ok(())
}

/// Captured *before* a calibration change so the review queue can resume where
/// the user was. Capturing afterwards captures the damage.
#[tauri::command]
pub fn capture_queue_position(
    project_id: String,
    current_asset_id: String,
    state: State<'_, AppState>,
) -> Result<QueuePosition, String> {
    state
        .catalog()?
        .capture_queue_position(&project_id, &current_asset_id)
        .map_err(error_message)
}

#[tauri::command]
pub fn restore_queue_position(
    project_id: String,
    position: QueuePosition,
    state: State<'_, AppState>,
) -> Result<String, String> {
    state
        .catalog()?
        .restore_queue_position(&project_id, &position)
        .map_err(error_message)
}

// --- D01-T027 consumer: diagnostics -----------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsView {
    pub diagnostics: Vec<Diagnostic>,
    pub counts: DiagnosticCounts,
}

#[tauri::command]
pub fn list_diagnostics(
    project_id: String,
    severity: Option<String>,
    kind_prefix: Option<String>,
    include_acknowledged: Option<bool>,
    state: State<'_, AppState>,
) -> Result<DiagnosticsView, String> {
    let catalog = state.catalog()?;
    let filter = DiagnosticFilter {
        severity_at_least: severity.as_deref().and_then(DiagnosticSeverity::parse),
        kind_prefix,
        include_acknowledged: include_acknowledged.unwrap_or(false),
    };
    Ok(DiagnosticsView {
        diagnostics: catalog
            .diagnostics(&project_id, &filter)
            .map_err(error_message)?,
        counts: catalog
            .diagnostic_counts(&project_id)
            .map_err(error_message)?,
    })
}

#[tauri::command]
pub fn acknowledge_diagnostic(
    project_id: String,
    diagnostic_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .catalog()?
        .acknowledge_diagnostic(&project_id, &diagnostic_id)
        .map_err(error_message)
}

/// Scans for source media that is not currently reachable. Called on project
/// open, so an unmounted card is a visible condition rather than a mystery.
#[tauri::command]
pub fn detect_offline_volumes(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<Diagnostic>, String> {
    state
        .catalog()?
        .detect_offline_volumes(&project_id)
        .map_err(error_message)
}

// --- D01-T023: edit plan and export -----------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditPlanOperationInput {
    pub asset_id: String,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub rationale: String,
}

#[tauri::command]
pub fn active_edit_plan(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Option<EditPlanRevision>, String> {
    state
        .catalog()?
        .active_edit_plan(&project_id)
        .map_err(error_message)
}

#[tauri::command]
pub fn edit_plan_revisions(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<EditPlanRevision>, String> {
    state
        .catalog()?
        .edit_plan_revisions(&project_id)
        .map_err(error_message)
}

#[tauri::command]
pub fn create_edit_plan_revision(
    project_id: String,
    operations: Vec<EditPlanOperationInput>,
    script_revision_id: Option<String>,
    expected_active_revision_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<EditPlanRevision, String> {
    let operations: Vec<EditPlanOperation> = operations
        .into_iter()
        .enumerate()
        .map(|(position, input)| EditPlanOperation {
            operation_id: String::new(),
            position: position as u32,
            asset_id: input.asset_id,
            source_in_seconds: input.source_in_seconds,
            source_out_seconds: input.source_out_seconds,
            rationale: input.rationale,
        })
        .collect();
    let revision = state
        .catalog()?
        .create_edit_plan_revision(
            &project_id,
            &operations,
            script_revision_id.as_deref(),
            expected_active_revision_id.as_deref(),
        )
        .map_err(error_message)?;
    state.sync_manager.wake_project(&project_id);
    Ok(revision)
}

#[tauri::command]
pub fn activate_edit_plan_revision(
    project_id: String,
    revision_id: String,
    expected_active_revision_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<EditPlanRevision, String> {
    let catalog = state.catalog()?;
    let candidate = catalog
        .edit_plan_revision(&revision_id)
        .map_err(error_message)?;
    if candidate.project_id != project_id {
        return Err(format!(
            "edit plan revision {revision_id} belongs to project {}, not {project_id}",
            candidate.project_id
        ));
    }
    let revision = catalog
        .activate_edit_plan_revision(&revision_id, expected_active_revision_id.as_deref())
        .map_err(error_message)?;
    state.sync_manager.wake_project(&revision.project_id);
    Ok(revision)
}

#[tauri::command]
pub fn validate_edit_plan(
    revision_id: String,
    state: State<'_, AppState>,
) -> Result<ValidationReport, String> {
    state
        .catalog()?
        .validate_edit_plan(&revision_id)
        .map_err(error_message)
}

/// Renders a low-resolution preview, emitting progress to the front end.
///
/// Progress events go out as they happen rather than as a final summary,
/// because a render of a long plan is exactly where a silent UI feels broken.
#[tauri::command]
pub fn render_edit_plan_preview(
    revision_id: String,
    max_height: Option<u32>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<PreviewResult, String> {
    use tauri::Emitter;
    let catalog = state.catalog()?;
    let mut emit = |progress: PreviewProgress| {
        let _ = app.emit("edit-plan-preview-progress", &progress);
    };
    catalog
        .render_preview(
            &PreviewRequest {
                revision_id,
                max_height: max_height.unwrap_or(540),
            },
            &mut emit,
            &|| false,
        )
        .map_err(error_message)
}

#[tauri::command]
pub fn export_edit_plan_otio(
    project_id: String,
    revision_id: String,
    output_path: String,
    expected_existing_digest: Option<String>,
    timeline_name: String,
    state: State<'_, AppState>,
) -> Result<OtioExportResult, String> {
    let catalog = state.catalog()?;
    let revision = catalog
        .edit_plan_revision(&revision_id)
        .map_err(error_message)?;
    if revision.project_id != project_id {
        return Err(format!(
            "edit plan revision {revision_id} belongs to project {}, not {project_id}",
            revision.project_id
        ));
    }
    let result = catalog
        .export_otio(&OtioExportRequest {
            revision_id,
            output_path,
            expected_existing_digest,
            timeline_name,
        })
        .map_err(error_message)?;
    state.sync_manager.wake_project(&revision.project_id);
    Ok(result)
}
