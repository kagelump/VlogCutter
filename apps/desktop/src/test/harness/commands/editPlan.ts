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
  ConfirmSourceRangeRequest,
  ConfirmedSourceRange,
  EditPlanRevision,
  OtioExportResult,
  PreviewResult,
  PreviewArtifactPage,
  PreviewArtifactStatus,
  SourceRangeConstraints,
  SourceInspectionRequest,
  SourceInspectionResult,
  SourceTrimRequest,
  SourceTrimSuggestion,
  ValidationReport,
} from "../../../chronologyTypes";
import type { JobBinding } from "../jobs";
import type { CommandTable, HarnessContext } from "../registry";
import { requireNumber, requireString } from "../registry";
import type { DesktopHarnessState } from "../state";

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

function constraintsForAsset(state: DesktopHarnessState, assetId: string): SourceRangeConstraints {
  const asset = state.assets[assetId];
  if (!asset) throw new Error(`Unknown asset: ${assetId}`);
  const declared = state.sourceRanges[assetId];
  if (declared) return declared;
  const empty: SourceRangeConstraints = {
    schemaReady: true,
    assetId,
    sourceFingerprint: asset.fingerprint,
    revision: 0,
    items: [],
    totalItems: 0,
    nextOffset: null,
    // The harness intentionally declares no clean ranges and performs no
    // feasibility calculation. Scenarios can provide the projection they need.
    feasiblePictureRanges: [],
    feasibleAudioRanges: [],
    pictureConflicts: [],
    audioConflicts: [],
  };
  state.sourceRanges[assetId] = empty;
  return empty;
}

function rangePage(
  constraints: SourceRangeConstraints,
  offset: number,
  limit: number,
): SourceRangeConstraints {
  if (!Number.isSafeInteger(offset) || offset < 0) throw new Error("offset must be a nonnegative integer");
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 200) throw new Error("limit must be an integer from 1 to 200");
  const items = constraints.items.slice(offset, offset + limit);
  return {
    ...constraints,
    items,
    totalItems: constraints.items.length,
    nextOffset: offset + items.length < constraints.items.length ? offset + items.length : null,
  };
}

function ownedAsset(state: DesktopHarnessState, projectId: string, assetId: string) {
  const asset = state.assets[assetId];
  if (!asset) throw new Error(`Unknown asset: ${assetId}`);
  if (asset.projectId !== projectId) {
    throw new Error(`asset ${assetId} belongs to project ${asset.projectId}, not ${projectId}`);
  }
  return asset;
}

function sourceRangeRequest(value: unknown): ConfirmSourceRangeRequest {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("confirm_source_range requires a request object");
  }
  const request = value as Partial<ConfirmSourceRangeRequest>;
  if (typeof request.assetId !== "string") throw new Error("request.assetId must be a string");
  if (typeof request.expectedRevision !== "number" || !Number.isSafeInteger(request.expectedRevision)) {
    throw new Error("request.expectedRevision must be an integer");
  }
  if (typeof request.expectedSourceFingerprint !== "string") {
    throw new Error("request.expectedSourceFingerprint must be a string");
  }
  if (request.decision !== "allow" && request.decision !== "exclude") {
    throw new Error("request.decision must be allow or exclude");
  }
  if (request.media !== "picture" && request.media !== "audio" && request.media !== "both") {
    throw new Error("request.media must be picture, audio, or both");
  }
  if (request.decisionOwner !== "human") throw new Error("request.decisionOwner must be human");
  for (const field of ["sourceInSeconds", "sourceOutSeconds"] as const) {
    if (typeof request[field] !== "number" || !Number.isFinite(request[field])) {
      throw new Error(`request.${field} must be a finite number`);
    }
  }
  if (typeof request.rationale !== "string" || !request.rationale.trim()) {
    throw new Error("request.rationale must be a nonempty string");
  }
  if (request.overridesEvidenceIds !== undefined &&
      (!Array.isArray(request.overridesEvidenceIds) || request.overridesEvidenceIds.some((id) => typeof id !== "string"))) {
    throw new Error("request.overridesEvidenceIds must be an array of strings");
  }
  if (request.inspectedEvidenceId) {
    throw new Error("The desktop harness cannot verify inspected media evidence; declare a media-free human decision.");
  }
  return request as ConfirmSourceRangeRequest;
}

function trimRequestFrom(value: unknown): SourceTrimRequest {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("suggest_source_trim requires a request object");
  }
  const request = value as Partial<SourceTrimRequest>;
  if (typeof request.assetId !== "string") throw new Error("request.assetId must be a string");
  if (typeof request.inspectedEvidenceId !== "string") throw new Error("request.inspectedEvidenceId must be a string");
  if (request.media !== "picture" && request.media !== "audio") {
    throw new Error("request.media must be picture or audio");
  }
  for (const field of ["sourceInSeconds", "sourceOutSeconds"] as const) {
    if (typeof request[field] !== "number" || !Number.isFinite(request[field])) {
      throw new Error(`request.${field} must be a finite number`);
    }
  }
  if (request.preferUtterances !== undefined && typeof request.preferUtterances !== "boolean") {
    throw new Error("request.preferUtterances must be a boolean");
  }
  return request as SourceTrimRequest;
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

  source_range_constraints: (args, { state }): SourceRangeConstraints => {
    const assetId = requireString(args, "assetId");
    const current = constraintsForAsset(state, assetId);
    const offset = args.offset === undefined ? 0 : requireNumber(args, "offset");
    const limit = args.limit === undefined ? 100 : requireNumber(args, "limit");
    return rangePage(current, offset, limit);
  },

  confirm_source_range: (args, { state, clock }): SourceRangeConstraints => {
    const projectId = requireString(args, "projectId");
    const request = sourceRangeRequest(args.request);
    const asset = ownedAsset(state, projectId, request.assetId);
    const current = constraintsForAsset(state, request.assetId);
    if (request.expectedRevision !== current.revision) {
      throw new Error(`source range constraints changed: expected revision ${request.expectedRevision}, found ${current.revision}`);
    }
    if (request.expectedSourceFingerprint !== asset.fingerprint || current.sourceFingerprint !== asset.fingerprint) {
      throw new Error("source range constraint source identity changed");
    }

    const constraintId = request.constraintId || `range_harness_${current.revision + 1}`;
    const prior = current.items.find((item) => item.constraintId === constraintId);
    if (prior && (prior.assetId !== request.assetId || prior.projectId !== projectId)) {
      throw new Error("constraint identity cannot be moved to another source/project");
    }
    const record: ConfirmedSourceRange = {
      constraintId,
      projectId,
      assetId: request.assetId,
      sourceFingerprint: asset.fingerprint,
      decision: request.decision,
      media: request.media,
      decisionOwner: "human",
      sourceInSeconds: request.sourceInSeconds,
      sourceOutSeconds: request.sourceOutSeconds,
      rationale: request.rationale,
      inspection: null,
      overridesEvidenceIds: request.overridesEvidenceIds ?? [],
      active: true,
      lastOperationId: `harness_source_range_operation_${current.revision + 1}`,
      updatedAtMillis: Date.parse(clock.iso()),
    };
    const items = prior
      ? current.items.map((item) => item.constraintId === constraintId ? record : item)
      : [...current.items, record];
    const updated: SourceRangeConstraints = {
      ...current,
      revision: current.revision + 1,
      items,
      totalItems: items.length,
      nextOffset: null,
    };
    state.sourceRanges[request.assetId] = updated;
    // Feasible ranges/conflicts remain scenario-declared. This adapter only
    // models operation lifecycle and never implements media-range math.
    return rangePage(updated, 0, 100);
  },

  revoke_source_range: (args, { state, clock }): SourceRangeConstraints => {
    const projectId = requireString(args, "projectId");
    const assetId = requireString(args, "assetId");
    ownedAsset(state, projectId, assetId);
    const constraintId = requireString(args, "constraintId");
    const expectedRevision = requireNumber(args, "expectedRevision");
    const rationale = requireString(args, "rationale");
    const current = constraintsForAsset(state, assetId);
    if (!Number.isSafeInteger(expectedRevision) || expectedRevision !== current.revision) {
      throw new Error(`source range constraints changed: expected revision ${expectedRevision}, found ${current.revision}`);
    }
    if (!rationale.trim()) throw new Error("rationale must be nonempty");
    const existing = current.items.find((item) => item.constraintId === constraintId);
    if (!existing) throw new Error(`source range constraint not found: ${constraintId}`);
    if (existing.projectId !== projectId || existing.assetId !== assetId) {
      throw new Error("constraint belongs to another source/project");
    }
    const items = current.items.map((item) => item.constraintId === constraintId
      ? {
          ...item,
          active: false,
          rationale,
          lastOperationId: `harness_source_range_operation_${current.revision + 1}`,
          updatedAtMillis: Date.parse(clock.iso()),
        }
      : item);
    const updated: SourceRangeConstraints = {
      ...current,
      revision: current.revision + 1,
      items,
      totalItems: items.length,
      nextOffset: null,
    };
    state.sourceRanges[assetId] = updated;
    return rangePage(updated, 0, 100);
  },

  suggest_source_trim: (args, { state }): SourceTrimSuggestion => {
    const request = trimRequestFrom(args.request);
    if (!state.assets[request.assetId]) throw new Error(`Unknown asset: ${request.assetId}`);
    const key = `${request.inspectedEvidenceId}:${request.media}`;
    const suggestion = state.sourceTrimSuggestions[key];
    if (!suggestion) throw new Error(`Scenario did not declare a source trim suggestion for ${key}`);
    return suggestion;
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

  preview_artifacts: (args, { state }): PreviewArtifactPage => {
    const projectId = requireString(args, "projectId");
    const revisionId = requireString(args, "revisionId");
    const revision = state.editPlans.revisions.find((item) => item.revisionId === revisionId);
    if (!revision) throw new Error(`Unknown edit plan revision: ${revisionId}`);
    if (revision.projectId !== projectId) throw new Error("edit plan revision belongs to another project");
    if (args.offset !== undefined && (!Number.isSafeInteger(args.offset) || (args.offset as number) < 0)) {
      throw new Error("offset must be a nonnegative integer");
    }
    if (args.limit !== undefined && (!Number.isSafeInteger(args.limit) || (args.limit as number) < 1 || (args.limit as number) > 100)) {
      throw new Error("limit must be an integer from 1 to 100");
    }
    const page = state.previewArtifactPages[revisionId];
    if (!page) throw new Error(`Scenario did not declare a preview artifact page for ${revisionId}`);
    return page;
  },

  preview_artifact_status: (args, { state }): PreviewArtifactStatus => {
    requireString(args, "projectId");
    const artifactId = requireString(args, "artifactId");
    if (args.verify !== undefined && typeof args.verify !== "boolean") throw new Error("verify must be a boolean");
    const status = state.previewArtifactStatuses[artifactId];
    if (!status) throw new Error(`Scenario did not declare preview artifact status for ${artifactId}`);
    if (status.artifact.projectId !== args.projectId) throw new Error("preview artifact belongs to another project");
    return status;
  },

  cancel_preview_artifact: (args, { state }): PreviewArtifactStatus => {
    requireString(args, "projectId");
    const artifactId = requireString(args, "artifactId");
    const status = state.previewArtifactStatuses[artifactId];
    if (!status) throw new Error(`Scenario did not declare preview artifact status for ${artifactId}`);
    if (status.artifact.projectId !== args.projectId) throw new Error("preview artifact belongs to another project");
    return status;
  },

  wait_preview_artifact: (args, { state }): PreviewArtifactStatus => {
    requireString(args, "projectId");
    const artifactId = requireString(args, "artifactId");
    if (args.timeoutSeconds !== undefined && (!Number.isSafeInteger(args.timeoutSeconds) || (args.timeoutSeconds as number) < 0)) {
      throw new Error("timeoutSeconds must be a nonnegative integer");
    }
    const status = state.previewArtifactStatuses[artifactId];
    if (!status) throw new Error(`Scenario did not declare preview artifact status for ${artifactId}`);
    if (status.artifact.projectId !== args.projectId) throw new Error("preview artifact belongs to another project");
    return status;
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
