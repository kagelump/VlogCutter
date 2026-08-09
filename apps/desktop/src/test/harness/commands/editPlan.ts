/**
 * Edit plans, validation, preview, and OTIO export.
 *
 * Like chronology, the interesting values are declared: which findings
 * validation returns, what the export reports. What the harness does implement
 * is the *lifecycle* the frontend depends on — revisions have identity, exactly
 * one is active, and a write carrying a stale expectation is refused. That
 * refusal is a frontend-visible contract, not a domain rule.
 */

import type { EditPlanRevision, OtioExportResult, PreviewResult, ValidationReport } from "../../../chronologyTypes";
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
