/**
 * Edit plans, validation, preview, and OTIO export.
 *
 * Like chronology, the interesting values are declared: which findings
 * validation returns, what the export reports. What the harness does implement
 * is the *lifecycle* the frontend depends on — revisions have identity, exactly
 * one is active, and a write carrying a stale expectation is refused. That
 * refusal is a frontend-visible contract, not a domain rule.
 */

import type {
  EditPlanRevision,
  OtioExportResult,
  PreviewResult,
  SourceInspectionRequest,
  SourceInspectionResult,
  ValidationReport,
} from "../../../chronologyTypes";
import type { JobBinding } from "../jobs";
import type { CommandTable, HarnessContext } from "../registry";
import { requireString } from "../registry";

interface OperationInput {
  assetId: string;
  sourceInSeconds: number;
  sourceOutSeconds: number;
  rationale: string;
}

function operationsFrom(value: unknown): OperationInput[] {
  if (!Array.isArray(value)) throw new Error("create_edit_plan_revision requires an operations array");
  return value.map((entry, index) => {
    const operation = entry as Partial<OperationInput>;
    if (typeof operation.assetId !== "string") throw new Error(`operations[${index}].assetId must be a string`);
    if (typeof operation.sourceInSeconds !== "number") throw new Error(`operations[${index}].sourceInSeconds must be a number`);
    if (typeof operation.sourceOutSeconds !== "number") throw new Error(`operations[${index}].sourceOutSeconds must be a number`);
    return {
      assetId: operation.assetId,
      sourceInSeconds: operation.sourceInSeconds,
      sourceOutSeconds: operation.sourceOutSeconds,
      rationale: operation.rationale ?? "",
    };
  });
}

/** A revision write must name the revision it believed was active. */
function assertNotStale(expected: unknown, activeId: string | null) {
  const expectation = typeof expected === "string" ? expected : null;
  if (expectation === activeId) return;
  throw new Error(
    `The edit plan changed since you loaded it: expected ${expectation ?? "no active revision"}, ` +
      `but ${activeId ?? "no revision"} is active. Reload and try again.`,
  );
}

export interface PreviewStep {
  position: number;
  failed?: string;
}

function inspectionRequestFrom(value: unknown): SourceInspectionRequest {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("inspect_source_window requires a request object");
  }
  const request = value as Partial<SourceInspectionRequest>;
  if (typeof request.assetId !== "string") throw new Error("request.assetId must be a string");
  if (request.representation !== "original" && request.representation !== "generatedProxy" && request.representation !== "cameraProxy") {
    throw new Error("request.representation must be a declared representation");
  }
  if (request.purpose !== "locator" && request.purpose !== "proposedBoundary" && request.purpose !== "inspectedBoundary") {
    throw new Error("request.purpose must be a declared inspection purpose");
  }
  for (const field of ["sourceInSeconds", "sourceOutSeconds", "sampleCount", "maxWidth"] as const) {
    if (typeof request[field] !== "number" || !Number.isFinite(request[field])) {
      throw new Error(`request.${field} must be a finite number`);
    }
  }

  // This is a deterministic transport fixture only. It deliberately does not
  // inspect media, decode requested PTS, read audio/transcripts, or implement
  // the production sampling/mapping algorithm.
  return request as SourceInspectionRequest;
}

function declaredInspectionFixture(request: SourceInspectionRequest): SourceInspectionResult {
  const before = request.beforeSeconds ?? 0;
  const start = request.sourceInSeconds - before;
  return {
    inspection: {
      evidenceId: "inspect_harness_fixture",
      recipe: "source-inspection.harness-fixture.v1",
      request,
      sourceFingerprint: "harness-source-fingerprint",
      representationFingerprint: "harness-representation-fingerprint",
      rotationDegrees: 0,
      mapping: {
        timeBaseNumerator: 1,
        timeBaseDenominator: 1,
        representationOriginPts: 0,
        sourceOffsetSeconds: request.sourceInSeconds,
        representationOffsetSeconds: 0,
        secondsScale: 1,
        precision: "harnessFixture",
        sourceFrameReduction: null,
      },
      windowInSeconds: start,
      windowOutSeconds: request.sourceOutSeconds + (request.afterSeconds ?? 0),
      frames: [{
        evidenceId: "inspect_harness_fixture:frame:0",
        representationPts: 0,
        sourceSeconds: request.sourceInSeconds,
        excerptSeconds: before,
        image: {
          path: "/harness/inspection/frame.jpg",
          blake3: "harness-fixture-only",
          byteLength: 1,
        },
      }],
      audio: request.includeAudio
        ? {
            state: "noAudioStream",
            playable: null,
            sampleRate: 48000,
            channels: 2,
            sourceInSamples: 0,
            sourceOutSamples: 0,
            streamSourceInSeconds: null,
            leadingGapSeconds: 0,
            waveform: [],
          }
        : null,
      utterances: [],
      availableFrameCount: 1,
      minAvailableFrameIntervalSeconds: null,
      maxAvailableFrameIntervalSeconds: null,
      maxUnsampledGapSeconds: Math.max(before, request.sourceOutSeconds + (request.afterSeconds ?? 0) - request.sourceInSeconds),
      limitations: [
        "Deterministic desktop harness fixture only; no media was probed or inspected.",
        "Frame, mapping, audio, and transcript fields are not source evidence and must not be used as media truth.",
      ],
      createdAt: "2000-01-01T00:00:00Z",
    },
    reused: false,
  };
}

export const editPlanCommands: CommandTable = {
  active_edit_plan: (args, { state }) => {
    requireString(args, "projectId");
    return state.editPlans.active;
  },

  edit_plan_revisions: (args, { state }) => {
    requireString(args, "projectId");
    return state.editPlans.revisions;
  },

  create_edit_plan_revision: (args, { state, clock }): EditPlanRevision => {
    const projectId = requireString(args, "projectId");
    const operations = operationsFrom(args.operations);
    if (state.editPlans.conflictMessage) throw new Error(state.editPlans.conflictMessage);
    assertNotStale(args.expectedActiveRevisionId, state.editPlans.active?.revisionId ?? null);

    const revision: EditPlanRevision = {
      revisionId: `revision_${state.editPlans.revisions.length + 1}`,
      projectId,
      scriptRevisionId: null,
      parentRevisionId: state.editPlans.active?.revisionId ?? null,
      active: false,
      operations: operations.map((operation, index) => ({
        operationId: `operation_${index + 1}`,
        position: index,
        ...operation,
      })),
      createdAt: clock.iso(),
    };
    state.editPlans.revisions.push(revision);
    return revision;
  },

  activate_edit_plan_revision: (args, { state }): EditPlanRevision => {
    requireString(args, "projectId");
    const revisionId = requireString(args, "revisionId");
    assertNotStale(args.expectedActiveRevisionId, state.editPlans.active?.revisionId ?? null);

    const target = state.editPlans.revisions.find((revision) => revision.revisionId === revisionId);
    if (!target) throw new Error(`Unknown edit plan revision: ${revisionId}`);
    state.editPlans.revisions = state.editPlans.revisions.map((revision) => ({
      ...revision,
      active: revision.revisionId === revisionId,
    }));
    state.editPlans.active = state.editPlans.revisions.find((revision) => revision.active) ?? null;
    return state.editPlans.active!;
  },

  /** Findings are declared per revision; an undeclared revision validates clean. */
  validate_edit_plan: (args, { state }): ValidationReport =>
    state.editPlans.validation[requireString(args, "revisionId")] ?? { findings: [] },

  inspect_source_window: (args): SourceInspectionResult => {
    const request = inspectionRequestFrom(args.request);
    return declaredInspectionFixture(request);
  },

  render_edit_plan_preview: (args, context: HarnessContext) => {
    const revisionId = requireString(args, "revisionId");
    const revision = context.state.editPlans.revisions.find((found) => found.revisionId === revisionId);
    if (!revision) throw new Error(`Unknown edit plan revision: ${revisionId}`);

    const result = (failures: PreviewResult["failures"]): PreviewResult => ({
      outputPath: `/Users/harness/Movies/previews/${revisionId}.mp4`,
      durationSeconds: revision.operations.reduce(
        (total, operation) => total + (operation.sourceOutSeconds - operation.sourceInSeconds),
        0,
      ),
      clipCount: revision.operations.length - failures.length,
      failures,
    });

    const controller = context.jobs.claimed("render_edit_plan_preview");
    if (!controller) return result([]);

    const failures: PreviewResult["failures"] = [];
    const binding: JobBinding<PreviewStep, { failures?: PreviewResult["failures"] }, PreviewResult> = {
      command: "render_edit_plan_preview",
      args: { revisionId },
      event: "edit-plan-preview-progress",
      progress: (step) => {
        if (step.failed) {
          failures.push({ position: step.position, assetId: step.failed, message: "clip failed to render" });
          return { type: "clipFailed", position: step.position, message: "clip failed to render" };
        }
        return {
          type: "clipCompleted",
          position: step.position,
          elapsedSeconds: step.position,
          estimateRemainingSeconds: Math.max(0, revision.operations.length - step.position),
        };
      },
      result: (outcome) => result(outcome?.failures ?? failures),
    };
    return controller.bind(binding);
  },

  export_edit_plan_otio: (args, { state }): OtioExportResult => {
    requireString(args, "projectId");
    const revisionId = requireString(args, "revisionId");
    const outputPath = requireString(args, "outputPath");
    requireString(args, "timelineName");
    const revision = state.editPlans.revisions.find((found) => found.revisionId === revisionId);
    if (!revision) throw new Error(`Unknown edit plan revision: ${revisionId}`);

    // An existing output is never silently replaced: the refusal names the
    // digest actually on disk, which is the contract the panel surfaces.
    const existing = state.editPlans.exportResult;
    if (existing && existing.outputPath === outputPath && args.expectedExistingDigest !== existing.outputDigest) {
      throw new Error(
        `${outputPath} already exists with digest ${existing.outputDigest}. Confirm the overwrite to replace it.`,
      );
    }
    const result: OtioExportResult = {
      outputPath,
      outputDigest: `digest_${revisionId}`,
      clipCount: revision.operations.length,
    };
    state.editPlans.exportResult = result;
    return result;
  },
};
