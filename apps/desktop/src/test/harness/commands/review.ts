/**
 * The review loop: reading a project's clips and writing judgments about them.
 *
 * Every mutation here changes canonical state, so the next `list_assets` — and
 * the queue, ribbon, and evidence shelf that read from it — observe the write.
 */

import type { Asset, AssetPatch, CameraMergePreview, CameraMergeReport } from "../../../types";
import { patchAsset, projectAssets, requireAsset, type DesktopHarnessState } from "../state";
import type { CommandTable } from "../registry";
import { requireBoolean, requireNumber, requireString } from "../registry";

function assetPatch(value: unknown): AssetPatch {
  if (!value || typeof value !== "object") throw new Error("update_asset requires a patch object");
  const patch = value as Partial<AssetPatch>;
  if (typeof patch.notes !== "string") throw new Error("patch.notes must be a string");
  if (!Array.isArray(patch.labels)) throw new Error("patch.labels must be an array");
  if (typeof patch.removed !== "boolean") throw new Error("patch.removed must be a boolean");
  if (typeof patch.rotationDegrees !== "number") throw new Error("patch.rotationDegrees must be a number");
  return patch as AssetPatch;
}

function cameraMergePreview(state: DesktopHarnessState, projectId: string, source: string, destination: string, requireDestination = true): CameraMergePreview {
  if (!source || !destination || source === destination) throw new Error("source and destination cameras must be different");
  const assets = projectAssets(state, projectId);
  const sourceExists = assets.some((asset) => asset.camera === source)
    || state.chronology.overview.sequences.some((sequence) => sequence.cameraKey === source);
  const destinationExists = assets.some((asset) => asset.camera === destination)
    || state.chronology.overview.sequences.some((sequence) => sequence.cameraKey === destination);
  if (!sourceExists) throw new Error(`camera not found in project: ${source}`);
  if (requireDestination && !destinationExists) throw new Error(`destination camera not found in project: ${destination}`);
  return {
    projectId,
    sourceCamera: source,
    destinationCamera: destination,
    affectedClipCount: assets.filter((asset) => asset.camera === source).length,
    affectedCaptureSequenceCount: state.chronology.overview.sequences.filter((sequence) => sequence.cameraKey === source).length,
    affectedMappingCount: (state.imports.cameraMappings[projectId] ?? []).filter((mapping) => mapping.camera === source).length,
    destinationCaptureSequenceCount: state.chronology.overview.sequences.filter((sequence) => sequence.cameraKey === destination).length,
  };
}

function applyCameraIdentity(state: DesktopHarnessState, projectId: string, source: string, destination: string, requireDestination = true): CameraMergeReport {
  const preview = cameraMergePreview(state, projectId, source, destination, requireDestination);
  for (const asset of projectAssets(state, projectId)) {
    if (asset.camera === source) patchAsset(state, asset.id, { camera: destination });
  }
  state.chronology.overview = {
    ...state.chronology.overview,
    sequences: state.chronology.overview.sequences.map((sequence) =>
      sequence.cameraKey === source ? { ...sequence, cameraKey: destination } : sequence,
    ),
  };
  for (const mapping of state.imports.cameraMappings[projectId] ?? []) {
    if (mapping.camera === source) mapping.camera = destination;
  }
  return {
    projectId,
    sourceCamera: source,
    destinationCamera: destination,
    movedClipCount: preview.affectedClipCount,
    movedCaptureSequenceCount: preview.affectedCaptureSequenceCount,
    movedMappingCount: preview.affectedMappingCount,
  };
}

export const reviewCommands: CommandTable = {
  list_assets: (args, { state }): Asset[] => projectAssets(state, requireString(args, "projectId")),

  get_asset: (args, { state }) => requireAsset(state, requireString(args, "assetId")),

  update_asset: (args, { state }) => {
    const assetId = requireString(args, "assetId");
    const patch = assetPatch(args.patch);
    // The catalog's optimistic check (issue #49): a whole patch built from a
    // stale read reverses the other writer rather than merely missing them.
    if (typeof patch.expectedRevision === "number") {
      const current = requireAsset(state, assetId);
      if (current.revision !== patch.expectedRevision) {
        throw new Error(
          `stale asset revision: expected revision ${patch.expectedRevision}, catalog is at ${current.revision}`,
        );
      }
    }
    return patchAsset(state, assetId, {
      notes: patch.notes,
      labels: [...patch.labels],
      removed: patch.removed,
      rotationDegrees: patch.rotationDegrees,
    });
  },

  create_note_revision: (args, { state }) =>
    patchAsset(state, requireString(args, "assetId"), { notes: requireString(args, "text") }),

  add_label: (args, { state }) => {
    const assetId = requireString(args, "assetId");
    const label = requireString(args, "label").trim();
    const current = requireAsset(state, assetId);
    if (!label) throw new Error("A label cannot be empty");
    const exists = current.labels.some((value) => value.toLowerCase() === label.toLowerCase());
    return exists ? current : patchAsset(state, assetId, { labels: [...current.labels, label] });
  },

  remove_label: (args, { state }) => {
    const assetId = requireString(args, "assetId");
    const label = requireString(args, "label").trim().toLowerCase();
    const current = requireAsset(state, assetId);
    return patchAsset(state, assetId, {
      labels: current.labels.filter((value) => value.toLowerCase() !== label),
    });
  },

  set_removed: (args, { state }) =>
    patchAsset(state, requireString(args, "assetId"), { removed: requireBoolean(args, "removed") }),

  set_rotation: (args, { state }) =>
    patchAsset(state, requireString(args, "assetId"), {
      rotationDegrees: requireNumber(args, "rotationDegrees"),
    }),

  set_clip_summary: (args, { state }) =>
    patchAsset(state, requireString(args, "assetId"), {
      title: requireString(args, "title").trim(),
      description: requireString(args, "description").trim(),
    }),

  /**
   * Orientation is probed from the original, so a clip whose original is gone
   * stays unprobed — which is what puts the stage's slate on screen.
   */
  refresh_asset_orientation: (args, { state }) => {
    const asset = requireAsset(state, requireString(args, "assetId"));
    if (!asset.originalAvailable) throw new Error(`Original is offline: ${asset.filename}`);
    return patchAsset(state, asset.id, { orientationProbed: true });
  },

  rename_camera: (args, { state }) => {
    const projectId = requireString(args, "projectId");
    const from = requireString(args, "from");
    const to = requireString(args, "to").trim();
    if (!to) throw new Error("A camera name cannot be empty");
    return applyCameraIdentity(state, projectId, from, to, false).movedClipCount;
  },

  preview_camera_merge: (args, { state }) => cameraMergePreview(
    state,
    requireString(args, "projectId"),
    requireString(args, "sourceCamera").trim(),
    requireString(args, "destinationCamera").trim(),
  ),

  merge_camera: (args, { state }) => applyCameraIdentity(
    state,
    requireString(args, "projectId"),
    requireString(args, "sourceCamera").trim(),
    requireString(args, "destinationCamera").trim(),
  ),

  /**
   * A clock event shifts every later clip from one camera. The real command
   * writes a replicated operation and re-solves chronology; here it moves the
   * per-asset offset, which is the part the review surface reads back.
   */
  apply_clock_event: (args, { state }) => {
    const projectId = requireString(args, "projectId");
    const camera = requireString(args, "camera");
    const startFilename = requireString(args, "startFilename");
    const offsetSeconds = requireNumber(args, "offsetSeconds");
    requireString(args, "label");
    for (const asset of projectAssets(state, projectId)) {
      if (asset.camera !== camera || asset.filename < startFilename) continue;
      patchAsset(state, asset.id, { offsetSeconds });
    }
    return null;
  },
};
