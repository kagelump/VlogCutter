/**
 * The chronology surface.
 *
 * Everything here is *declared*, never computed. `rcdvlog-core` owns the
 * solver; a second one in TypeScript could disagree with it while looking
 * entirely convincing, and every CUJ written against it would then be evidence
 * for nothing. So a scenario states the overview the backend returns and, keyed
 * by the sequence an edit targets, the overview it returns afterwards.
 *
 * What the CUJs prove is the frontend's safety contract — preview before write,
 * capture the user's place before mutating, refresh after, restore where they
 * were — which is entirely a frontend property and entirely testable here.
 */

import type {
  ChronologyOverview,
  ClockEpoch,
  Contradiction,
  Diagnostic,
  DiagnosticsView,
  QueuePosition,
} from "../../../chronologyTypes";
import { projectAssets, type DesktopHarnessState } from "../state";
import type { CommandTable } from "../registry";
import { requireNumber, requireString } from "../registry";

function overviewOf(state: DesktopHarnessState): ChronologyOverview {
  return state.chronology.overview;
}

function epochFrom(value: unknown): ClockEpoch {
  if (!value || typeof value !== "object") throw new Error("A clock epoch object is required");
  const epoch = value as Partial<ClockEpoch>;
  if (typeof epoch.captureSequenceId !== "string" || !epoch.captureSequenceId) {
    throw new Error("A clock epoch needs a capture sequence");
  }
  if (typeof epoch.offsetSeconds !== "number") throw new Error("A clock epoch needs an offset");
  return {
    id: epoch.id || "",
    captureSequenceId: epoch.captureSequenceId,
    startCounter: epoch.startCounter ?? 0,
    endCounter: epoch.endCounter ?? null,
    offsetSeconds: epoch.offsetSeconds,
    confidence: epoch.confidence ?? 1,
    evidence: epoch.evidence ?? [],
    label: epoch.label ?? "",
  };
}

function visibleDiagnostics(state: DesktopHarnessState, includeAcknowledged: boolean): Diagnostic[] {
  return state.diagnostics.filter((found) => includeAcknowledged || found.acknowledgedAt === null);
}

function counts(diagnostics: Diagnostic[]): DiagnosticsView["counts"] {
  return {
    errors: diagnostics.filter((found) => found.severity === "error").length,
    warnings: diagnostics.filter((found) => found.severity === "warning").length,
    infos: diagnostics.filter((found) => found.severity === "info").length,
  };
}

export const chronologyCommands: CommandTable = {
  chronology_overview: (args, { state }): ChronologyOverview => {
    requireString(args, "projectId");
    return overviewOf(state);
  },

  explain_chronology: (args, { state }) => state.chronology.explanations[requireString(args, "assetId")] ?? null,

  /**
   * What a candidate epoch would do, computed nowhere. The scenario declares
   * the contradictions this sequence's edit produces; an undeclared sequence
   * previews clean.
   */
  preview_clock_epoch: (args, { state }): Contradiction[] => {
    requireString(args, "projectId");
    const epoch = epochFrom(args.epoch);
    return state.chronology.previewsByEdit[epoch.captureSequenceId] ?? [];
  },

  set_clock_epoch: (args, { state }): ClockEpoch => {
    requireString(args, "projectId");
    const epoch = epochFrom(args.epoch);
    const stored: ClockEpoch = {
      ...epoch,
      id: epoch.id || `epoch_${epoch.captureSequenceId}_${state.chronology.overview.epochs.length + 1}`,
    };
    // The scenario's declared post-edit overview, when it has one. Otherwise
    // the correction is recorded and the ordering is left exactly as declared —
    // the harness will not invent a new order.
    const outcome = state.chronology.outcomesByEdit[epoch.captureSequenceId];
    state.chronology.overview = outcome
      ? { ...outcome, epochs: [...outcome.epochs, stored] }
      : { ...state.chronology.overview, epochs: [...state.chronology.overview.epochs, stored] };
    return stored;
  },

  clear_clock_epoch: (args, { state }) => {
    requireString(args, "projectId");
    const epochId = requireString(args, "epochId");
    state.chronology.overview = {
      ...state.chronology.overview,
      epochs: state.chronology.overview.epochs.filter((epoch) => epoch.id !== epochId),
    };
    return null;
  },

  split_capture_sequence: (args, { state }) => {
    requireString(args, "projectId");
    const sourceSequenceId = requireString(args, "sourceSequenceId");
    const atCounter = requireNumber(args, "atCounter");
    const label = requireString(args, "label");
    const source = state.chronology.overview.sequences.find((sequence) => sequence.id === sourceSequenceId);
    if (!source) throw new Error(`Unknown capture sequence: ${sourceSequenceId}`);
    const created = { ...source, id: `${sourceSequenceId}_split_${atCounter}`, label };
    state.chronology.overview = {
      ...state.chronology.overview,
      sequences: [...state.chronology.overview.sequences, created],
      proposedSplits: state.chronology.overview.proposedSplits.filter(
        (proposal) => !(proposal.captureSequenceId === sourceSequenceId && proposal.atCounter === atCounter),
      ),
    };
    return created;
  },

  merge_capture_sequences: (args, { state }) => {
    requireString(args, "projectId");
    const fromSequenceId = requireString(args, "fromSequenceId");
    requireString(args, "intoSequenceId");
    state.chronology.overview = {
      ...state.chronology.overview,
      sequences: state.chronology.overview.sequences.filter((sequence) => sequence.id !== fromSequenceId),
    };
    return null;
  },

  set_clip_time_exception: (args, { state }) => {
    requireString(args, "projectId");
    const exception = {
      assetId: requireString(args, "assetId"),
      tripTime: requireString(args, "tripTime"),
      reason: requireString(args, "reason"),
    };
    state.chronology.overview = {
      ...state.chronology.overview,
      exceptions: [
        ...state.chronology.overview.exceptions.filter((found) => found.assetId !== exception.assetId),
        exception,
      ],
    };
    return exception;
  },

  clear_clip_time_exception: (args, { state }) => {
    requireString(args, "projectId");
    const assetId = requireString(args, "assetId");
    state.chronology.overview = {
      ...state.chronology.overview,
      exceptions: state.chronology.overview.exceptions.filter((found) => found.assetId !== assetId),
    };
    return null;
  },

  set_trip_phase: (args, { state }) => {
    requireString(args, "projectId");
    const phase = args.phase as ChronologyOverview["phases"][number];
    if (!phase || typeof phase !== "object") throw new Error("A trip phase object is required");
    const stored = { ...phase, id: phase.id || `phase_${state.chronology.overview.phases.length + 1}` };
    state.chronology.overview = {
      ...state.chronology.overview,
      phases: [...state.chronology.overview.phases.filter((found) => found.id !== stored.id), stored],
    };
    return stored;
  },

  remove_trip_phase: (args, { state }) => {
    requireString(args, "projectId");
    const phaseId = requireString(args, "phaseId");
    state.chronology.overview = {
      ...state.chronology.overview,
      phases: state.chronology.overview.phases.filter((phase) => phase.id !== phaseId),
    };
    return null;
  },

  set_event_order_constraint: (args, { state }) => {
    requireString(args, "projectId");
    const constraint = {
      id: `constraint_${state.chronology.overview.constraints.length + 1}`,
      earlierAssetId: requireString(args, "earlierAssetId"),
      laterAssetId: requireString(args, "laterAssetId"),
      strength: requireString(args, "strength") as "hard" | "soft",
      source: requireString(args, "source"),
    };
    state.chronology.overview = {
      ...state.chronology.overview,
      constraints: [...state.chronology.overview.constraints, constraint],
    };
    return constraint;
  },

  clear_event_order_constraint: (args, { state }) => {
    requireString(args, "projectId");
    const constraintId = requireString(args, "constraintId");
    state.chronology.overview = {
      ...state.chronology.overview,
      constraints: state.chronology.overview.constraints.filter((found) => found.id !== constraintId),
    };
    return null;
  },

  /**
   * The user's place, taken before a change and handed back after it. The
   * predecessors are what make the place survive a reorder; here they are the
   * clips currently ahead of the anchor.
   */
  capture_queue_position: (args, { state }): QueuePosition => {
    const projectId = requireString(args, "projectId");
    const currentAssetId = requireString(args, "currentAssetId");
    const ordered = projectAssets(state, projectId).filter((asset) => !asset.removed);
    const index = ordered.findIndex((asset) => asset.id === currentAssetId);
    const position: QueuePosition = {
      anchorAssetId: currentAssetId,
      orderedPredecessors: index > 0 ? ordered.slice(0, index).map((asset) => asset.id) : [],
    };
    state.chronology.capturedPositions.push(position);
    return position;
  },

  restore_queue_position: (args, { state }) => {
    requireString(args, "projectId");
    const position = args.position as QueuePosition | undefined;
    if (!position?.anchorAssetId) throw new Error("A queue position is required");
    // The scenario can say the anchor did not survive, in which case the
    // backend resumes somewhere else and the UI must follow it.
    return state.chronology.restoreTo ?? position.anchorAssetId;
  },

  list_diagnostics: (args, { state }): DiagnosticsView => {
    requireString(args, "projectId");
    const includeAcknowledged = args.includeAcknowledged === true;
    const diagnostics = visibleDiagnostics(state, includeAcknowledged);
    return { diagnostics, counts: counts(diagnostics) };
  },

  acknowledge_diagnostic: (args, { state, clock }) => {
    requireString(args, "projectId");
    const diagnosticId = requireString(args, "diagnosticId");
    state.diagnostics = state.diagnostics.map((found) =>
      found.id === diagnosticId ? { ...found, acknowledgedAt: clock.iso() } : found,
    );
    if (!state.acknowledgedDiagnostics.includes(diagnosticId)) {
      state.acknowledgedDiagnostics.push(diagnosticId);
    }
    return null;
  },

  /**
   * Offline volumes are a declared condition that stays true only while it is
   * true: a diagnostic whose clips have been relinked is no longer reported,
   * so the repair is observable through the same command that raised it.
   */
  detect_offline_volumes: (args, { state }): Diagnostic[] => {
    const projectId = requireString(args, "projectId");
    const offline = new Set(
      projectAssets(state, projectId)
        .filter((asset) => !asset.originalAvailable)
        .map((asset) => asset.id),
    );
    return state.diagnostics.filter((found) => {
      if (found.kind !== "media.volume_offline") return false;
      const context = found.context as { assetIds?: string[] } | null;
      const assetIds = context?.assetIds ?? [];
      return assetIds.length === 0 || assetIds.some((assetId) => offline.has(assetId));
    });
  },
};
