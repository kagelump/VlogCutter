/**
 * Card detection, archive roots, preflight, and the import run.
 *
 * The rule this module keeps: **preflight never mutates, and a run adds exactly
 * the items its plan classified as new.** Those two together are what make the
 * incremental case — the same card inserted twice with footage added between —
 * checkable at all.
 *
 * Nothing here is evidence for real copying, hashing, or removable-volume
 * detection. `docs/tasks/import/EPIC.md` keeps its physical gate: one real card,
 * inserted twice, and an archive moved between two physical volumes.
 */

import type {
  ArchiveRoot,
  Asset,
  CameraMapping,
  CameraEvidence,
  CardCandidate,
  ImportPreflight,
  ImportPreviewItem,
  ImportProgress,
  ImportRollbackPreview,
  ImportRunReport,
  ImportSessionDetail,
  PreflightItem,
  ResolvedImportCamera,
} from "../../../types";
import { buildArchiveRoot } from "../builders";
import type { JobBinding } from "../jobs";
import { projectAssets, putAsset, type DesktopHarnessState } from "../state";
import type { CommandTable, HarnessContext } from "../registry";
import { requireBoolean, requireString } from "../registry";

// --- #71: resolving and remembering a camera identity ---
//
// Mirrors `core/rcdvlog-core/src/cameras/mapping.rs` and
// `preflight.rs::resolve_import_camera` closely enough that the same scenario
// sequence exercises the same decisions, without re-implementing the Rust
// solver: evidence is the card's folder-layout hint, the dominant filename
// prefix among the incoming new clips, and representative EXIF make/model.
// It is looked up or recorded only when all three are present. See that
// module's doc comment for why mount path and volume label are excluded.

function evidenceIsStrong(evidence: CameraEvidence): boolean {
  return evidence.folderHint !== "" && evidence.filenamePrefix !== "" && evidence.exifHint !== "";
}

/** The stem up to its first digit run, trimmed and upper-cased — the same
 * rule `preflight.rs::filename_prefix` applies. */
function derivePrefixFromFilename(filename: string): string {
  const stem = filename.includes(".") ? filename.slice(0, filename.lastIndexOf(".")) : filename;
  const digitIndex = Array.from(stem).findIndex((character) => character >= "0" && character <= "9");
  const rawPrefix = digitIndex === -1 ? stem : stem.slice(0, digitIndex);
  const trimmed = rawPrefix.replace(/^[-_. ]+|[-_. ]+$/g, "");
  return (trimmed || stem).toUpperCase();
}

/** The most common value, ties broken by whichever sorts first — deterministic
 * regardless of iteration order, same as `preflight.rs::mode`. */
function mode(values: string[]): string {
  const counts = new Map<string, number>();
  for (const value of values) counts.set(value, (counts.get(value) ?? 0) + 1);
  let best: string | null = null;
  let bestCount = 0;
  for (const [value, count] of [...counts.entries()].sort(([left], [right]) => left.localeCompare(right))) {
    if (count > bestCount) {
      best = value;
      bestCount = count;
    }
  }
  return best ?? "";
}

function cameraEvidenceFor(card: CardCandidate | null, newFilenames: string[], exifHint: string): CameraEvidence {
  return {
    folderHint: card?.cameraHint.trim() ?? "",
    filenamePrefix: mode(newFilenames.map(derivePrefixFromFilename)),
    exifHint,
  };
}

function remembered(state: DesktopHarnessState, projectId: string, evidence: CameraEvidence): string | null {
  if (!evidenceIsStrong(evidence)) return null;
  const mappings = state.imports.cameraMappings[projectId] ?? [];
  const match = mappings.find(
    (mapping) => mapping.folderHint === evidence.folderHint
      && mapping.filenamePrefix === evidence.filenamePrefix
      && mapping.exifHint === evidence.exifHint,
  );
  return match?.camera ?? null;
}

/** Records (or updates) the camera one evidence signature should default to.
 * A no-op for weak evidence or an unusable camera name, same as the core
 * store's own gate. */
function rememberCameraMapping(
  state: DesktopHarnessState,
  projectId: string,
  evidence: CameraEvidence,
  camera: string,
  nowIso: string,
): void {
  const trimmed = camera.trim();
  if (!evidenceIsStrong(evidence) || trimmed === "" || trimmed === "Unknown camera") return;
  const mappings = state.imports.cameraMappings[projectId] ?? [];
  const existing = mappings.find(
    (mapping) => mapping.folderHint === evidence.folderHint
      && mapping.filenamePrefix === evidence.filenamePrefix
      && mapping.exifHint === evidence.exifHint,
  );
  if (existing) {
    existing.camera = trimmed;
    existing.updatedAt = nowIso;
    return;
  }
  const id = `cammap_${projectId}_${evidence.folderHint}_${evidence.filenamePrefix}`
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_");
  state.imports.cameraMappings[projectId] = [
    ...mappings,
    { id, folderHint: evidence.folderHint, filenamePrefix: evidence.filenamePrefix, exifHint: evidence.exifHint, camera: trimmed, createdAt: nowIso, updatedAt: nowIso },
  ];
}

/** Resolves the one camera identity a plan uses — user override, then a
 * remembered mapping, then plain per-card detection — the same precedence
 * `preflight.rs::resolve_import_camera` documents. */
function resolveCameraIdentity(
  state: DesktopHarnessState,
  projectId: string,
  card: CardCandidate | null,
  newClips: Asset[],
  evidence: CameraEvidence,
  cameraOverride: string | null,
): ResolvedImportCamera {
  const evidenceStrong = evidenceIsStrong(evidence);

  const trimmedOverride = cameraOverride?.trim();
  if (trimmedOverride) {
    return { camera: trimmedOverride, source: "userSelected", evidenceStrong };
  }

  const rememberedCamera = remembered(state, projectId, evidence);
  if (rememberedCamera) {
    return { camera: rememberedCamera, source: "remembered", evidenceStrong };
  }

  const detected = newClips[0]?.camera ?? card?.cameraHint ?? "";
  return {
    camera: detected || "Unknown camera",
    source: detected && detected !== "Unknown camera" ? "detected" : "unknown",
    evidenceStrong,
  };
}

/**
 * Build the plan for a source, from what the scenario said is on the card and
 * what the project already holds. Pure: it reads state and writes none.
 */
function preflight(
  state: DesktopHarnessState,
  projectId: string,
  sourcePath: string,
  archiveRootId: string | null,
  cameraOverride: string | null,
): ImportPreflight {
  const declared = state.imports.preflights[sourcePath];
  if (declared) return declared;

  const card = state.cards.find((candidate) => candidate.mountPath === sourcePath) ?? null;
  const additions = state.imports.additions[sourcePath] ?? [];
  const known = projectAssets(state, projectId);
  const newClips = additions.filter((asset) => !state.assets[asset.id]);
  const archiveRoot = state.archiveRoots.find((root) => root.id === archiveRootId) ?? null;
  const bytesToCopy = newClips.reduce((total, asset) => total + asset.sizeBytes, 0);
  const availableBytes = archiveRoot?.availableBytes ?? null;
  const roomless = availableBytes !== null && availableBytes < bytesToCopy;
  const evidence = cameraEvidenceFor(
    card,
    newClips.map((asset) => asset.filename),
    state.imports.cameraExifHints[sourcePath] ?? "",
  );
  const cameraIdentity = resolveCameraIdentity(state, projectId, card, newClips, evidence, cameraOverride);
  const newCamera = cameraIdentity.camera;
  const existingCameras = new Set(known.map((asset) => asset.camera));
  const matchesExistingSequence = newClips.length > 0 && existingCameras.has(newCamera);
  const sequenceLabel = `${newCamera} card`;
  const firstNew = newClips[0];
  const lastNew = newClips.at(-1);

  const items: PreflightItem[] = additions.map((asset) => ({
    path: `${sourcePath}/${asset.filename}`,
    relativePath: asset.filename,
    filename: asset.filename,
    sizeBytes: asset.sizeBytes,
    classification: state.assets[asset.id] ? "knownVerified" : "new",
    assetId: state.assets[asset.id] ? asset.id : null,
    error: "",
    // A cheap stand-in for what real preflight computes from one `stat`: the
    // harness has no filesystem, so it reuses the scenario's own capture time
    // rather than modelling a distinct mtime.
    fingerprint: asset.fingerprint,
    filesystemModifiedAt: asset.bestTime ?? asset.capturedAtRaw ?? null,
  }));

  return {
    sourcePath,
    card,
    cameraIdentity,
    cameraEvidence: evidence,
    destination: {
      copies: card !== null,
      archiveRoot,
      resolvedPath: archiveRoot ? `${archiveRoot.localMountPath}/${card?.label ?? "import"}` : sourcePath,
      summary: archiveRoot
        ? `Originals are copied to “${archiveRoot.displayName}”, and stay on the card too`
        : "Originals stay where they are",
      bytesToCopy,
      availableBytes,
      ready: !roomless && (card === null || archiveRoot !== null),
      blockedReason: roomless
        ? `“${archiveRoot?.displayName}” does not have room for this import`
        : card !== null && !archiveRoot
          ? "Choose where the originals should be copied"
          : "",
    },
    scanned: additions.length,
    newClips: newClips.length,
    knownClips: additions.length - newClips.length,
    relinkable: 0,
    missingProxies: known.filter((asset) => !asset.proxyAvailable).length,
    collisions: 0,
    unreadable: 0,
    sequencePreview: {
      newClipCount: newClips.length,
      existingSequenceCount: existingCameras.size,
      proposedSequenceCount: newClips.length > 0 ? 1 : 0,
      matchedExistingSequenceCount: matchesExistingSequence ? 1 : 0,
      suggestedNewSequenceCount: newClips.length > 0 && !matchesExistingSequence ? 1 : 0,
      boundaries: [],
      runs: newClips.length > 0 ? [{
        firstRelativePath: firstNew!.filename,
        lastRelativePath: lastNew!.filename,
        clipCount: newClips.length,
        cameraKey: newCamera,
        volumeHint: card?.sessionId ?? sourcePath,
        filenamePrefix: evidence.filenamePrefix,
        decision: matchesExistingSequence ? "extendExisting" : "proposeNew",
        matchedExistingSequenceId: matchesExistingSequence ? `sequence_${newCamera.toLowerCase().replaceAll(" ", "_")}` : null,
        matchedExistingSequenceLabel: matchesExistingSequence ? sequenceLabel : null,
        comparedExistingSequenceId: null,
        comparedExistingSequenceLabel: null,
        // The harness never models a comparison against an existing
        // sequence, so this run is always routine either way (it matches or
        // it's brand new — never a disagreement worth reviewing).
        needsReview: false,
        reason: matchesExistingSequence
          ? `This run matches the camera, card, and filename evidence for “${sequenceLabel}”.`
          : `No existing capture sequence is available for ${newCamera}.`,
        evidence: [],
      }] : [],
    },
    items,
  };
}

interface RunTally {
  projectId: string;
  imported: string[];
  pending: Asset[];
  bytesCopied: number;
}

interface RunOutcome {
  cancelled?: boolean;
  proxyCancelled?: boolean;
}

/**
 * Whether the plan's own source is still in the slot, matched on session
 * identity rather than mount path — the harness's stand-in for
 * `import_source_present` in `preflight.rs`. A test that ejects a card can
 * therefore never receive a clean report, which is the invariant #67 rests on.
 */
function sourcePresent(state: DesktopHarnessState, plan: ImportPreflight): boolean {
  if (!plan.card) return true;
  return state.cards.some((card) => card.sessionId === plan.card!.sessionId);
}

/** Only a user selection or a remembered mapping forces every new asset's
 * camera, mirroring `run_import`'s own `camera_override` gate in
 * `preflight.rs` — plain detection leaves each clip's own declared camera
 * alone. */
function uniformCameraFor(plan: ImportPreflight): string | null {
  const identity = plan.cameraIdentity;
  if (!identity) return null;
  return identity.source === "userSelected" || identity.source === "remembered" ? identity.camera : null;
}

function stepProgress(state: DesktopHarnessState, tally: RunTally, plan: ImportPreflight): ImportProgress {
  const next = tally.pending.shift();
  if (!next) throw new Error("The import has no pending clips left to report");
  const camera = uniformCameraFor(plan);
  const committed = camera ? { ...next, camera } : next;
  putAsset(state, committed);
  tally.imported.push(next.id);
  tally.bytesCopied += next.sizeBytes;
  return {
    projectId: tally.projectId,
    stage: "copying",
    completed: tally.imported.length,
    total: plan.newClips,
    currentFile: next.filename,
    completedBytes: tally.bytesCopied,
    totalBytes: plan.destination.bytesToCopy,
    asset: committed,
  };
}

function runReport(
  state: DesktopHarnessState,
  tally: RunTally,
  plan: ImportPreflight,
  cancelled: boolean,
  makeProxies: boolean,
  proxyCancelled = false,
): ImportRunReport {
  // A source that left stops the run; it does not cancel it and it produces no
  // failed files. Proxies are skipped for the same reason the desktop command
  // skips them: the reconnect prompt is the thing the user needs next.
  const sourceDisconnected = !sourcePresent(state, plan);
  const sessionId = `import_${Object.keys(state.imports.sessions).length + 1}`;
  const session: ImportSessionDetail = {
    id: sessionId, projectId: tally.projectId, sourcePath: plan.sourcePath,
    destinationMode: plan.destination.copies ? "archive_copy" : "reference",
    destinationRootId: plan.destination.archiveRoot?.id ?? null,
    destinationName: plan.destination.archiveRoot?.displayName ?? "Originals stay in place",
    destinationPath: plan.destination.resolvedPath,
    imported: tally.imported.length, relinked: 0, skipped: plan.knownClips, copied: tally.imported.length,
    bytesCopied: tally.bytesCopied, cancelled, failures: [], rolledBackAt: null, rolledBackAssetCount: 0,
    createdAt: "2026-08-10T12:00:00.000Z",
    files: plan.items.map((item) => ({ assetId: tally.imported.find((id) => state.assets[id]?.filename === item.filename) ?? item.assetId ?? null, path: item.path, filename: item.filename, classification: item.classification, result: item.classification === "new" ? "completed" : "left_alone", timestamp: null, durationSeconds: null, error: null })),
  };
  state.imports.sessions[sessionId] = session;
  return {
    outcome: {
      imported: tally.imported.length,
      importedAssetIds: [...tally.imported],
      relinked: 0,
      relinkedAssetIds: [],
      skipped: plan.knownClips,
      copied: tally.imported.length,
      bytesCopied: tally.bytesCopied,
      cancelled,
      sourceDisconnected,
      failures: [],
      warnings: [],
      cameraMappingWarning: null,
    },
    proxies: makeProxies && !sourceDisconnected
      ? { generated: tally.imported.length, skipped: 0, cancelled: cancelled || proxyCancelled, failures: [] }
      : null,
    files: plan.items.map((item) => {
      const asset = (item.assetId ? state.assets[item.assetId] : undefined)
        ?? Object.values(state.assets).find((candidate) => candidate.filename === item.filename && candidate.sizeBytes === item.sizeBytes);
      return {
        path: item.path,
        timestamp: asset?.bestTime ?? asset?.capturedAtRaw ?? null,
        durationSeconds: asset?.durationSeconds ?? null,
      };
    }),
    session,
  };
}

export const importCommands: CommandTable = {
  detect_mounted_cards: (_args, { state }): CardCandidate[] => state.cards,

  inspect_import_source: (args, { state }) => {
    const path = requireString(args, "path");
    return state.cards.find((card) => card.mountPath === path) ?? null;
  },

  list_archive_roots: (_args, { state }) => state.archiveRoots,

  create_archive_root: (args, { state, clock }): ArchiveRoot => {
    const displayName = requireString(args, "displayName");
    const path = requireString(args, "path");
    const root = buildArchiveRoot(displayName, { localMountPath: path, createdAt: clock.iso() });
    state.archiveRoots = [root, ...state.archiveRoots.filter((existing) => existing.id !== root.id)];
    return root;
  },

  bind_archive_root: (args, { state }): ArchiveRoot => {
    const archiveRootId = requireString(args, "archiveRootId");
    const path = requireString(args, "path");
    const root = state.archiveRoots.find((existing) => existing.id === archiveRootId);
    if (!root) throw new Error(`Unknown archive root: ${archiveRootId}`);
    const rebound = { ...root, localMountPath: path, available: true };
    state.archiveRoots = state.archiveRoots.map((existing) => (existing.id === rebound.id ? rebound : existing));
    return rebound;
  },

  set_import_preferences: (args, { state }) => {
    state.settings.makeProxiesOnImport = requireBoolean(args, "makeProxies");
    const archiveRootId = args.archiveRootId;
    if (archiveRootId !== null && typeof archiveRootId !== "string") {
      throw new Error("set_import_preferences archiveRootId must be a string or null");
    }
    state.settings.lastImportArchiveRootId = archiveRootId;
    const cardSessionId = args.cardSessionId;
    if (cardSessionId !== null && typeof cardSessionId !== "string") {
      throw new Error("set_import_preferences cardSessionId must be a string or null");
    }
    if (cardSessionId && archiveRootId) {
      state.settings.importArchiveRootIdsByCardSession[cardSessionId] = archiveRootId;
    }
    return { ...state.settings, mediaOrigin: state.mediaOrigin };
  },

  /** Reads the project and the card; writes nothing. The CUJ asserts that. */
  preflight_import: (args, { state }): ImportPreflight => {
    const projectId = requireString(args, "projectId");
    const sourcePath = requireString(args, "sourcePath");
    const archiveRootId = typeof args.archiveRootId === "string" ? args.archiveRootId : null;
    const cameraOverride = typeof args.cameraOverride === "string" ? args.cameraOverride : null;
    return preflight(state, projectId, sourcePath, archiveRootId, cameraOverride);
  },

  /** Every camera identity this project already has footage under (#71). */
  known_project_cameras: (args, { state }): string[] => {
    const projectId = requireString(args, "projectId");
    const cameras = new Set(
      projectAssets(state, projectId)
        .map((asset) => asset.camera)
        .filter((camera) => camera && camera !== "Unknown camera"),
    );
    return [...cameras].sort();
  },

  list_camera_mappings: (args, { state }): CameraMapping[] => {
    const projectId = requireString(args, "projectId");
    return [...(state.imports.cameraMappings[projectId] ?? [])].sort((left, right) =>
      left.camera === right.camera ? left.id.localeCompare(right.id) : left.camera.localeCompare(right.camera),
    );
  },

  rename_camera_mapping: (args, { state, clock }): CameraMapping => {
    const projectId = requireString(args, "projectId");
    const mappingId = requireString(args, "mappingId");
    const camera = requireString(args, "camera").trim();
    if (!camera) throw new Error("a camera mapping needs a name");
    const mappings = state.imports.cameraMappings[projectId] ?? [];
    const mapping = mappings.find((candidate) => candidate.id === mappingId);
    if (!mapping) throw new Error(`camera mapping not found: ${mappingId}`);
    mapping.camera = camera;
    mapping.updatedAt = clock.iso();
    return mapping;
  },

  clear_camera_mapping: (args, { state }): null => {
    const projectId = requireString(args, "projectId");
    const mappingId = requireString(args, "mappingId");
    state.imports.cameraMappings[projectId] = (state.imports.cameraMappings[projectId] ?? []).filter(
      (mapping) => mapping.id !== mappingId,
    );
    return null;
  },

  run_smart_import: (args, context: HarnessContext): ImportRunReport | Promise<unknown> => {
    const projectId = requireString(args, "projectId");
    const plan = args.plan as ImportPreflight | undefined;
    if (!plan?.sourcePath) throw new Error("run_smart_import requires the plan it previewed");
    requireBoolean(args, "verifyCopies");
    const makeProxies = requireBoolean(args, "makeProxies");
    if (!plan.destination.ready) throw new Error(plan.destination.blockedReason || "This destination is not ready");

    context.state.imports.lastRunSourcePath = plan.sourcePath;
    const additions = context.state.imports.additions[plan.sourcePath] ?? [];
    const tally: RunTally = {
      projectId,
      imported: [],
      // Exactly the clips the plan classified as new, and no others.
      pending: additions.filter((asset) => !context.state.assets[asset.id]),
      bytesCopied: 0,
    };
    // A completed import driven by a real user decision is the project's
    // best current statement of what this card's evidence means — same
    // `UserSelected`-only gate as `run_import` in `preflight.rs`. A plain
    // `detected` guess is never recorded: it can differ from what a real,
    // EXIF-aware ingest would assign, and recording it would let a later
    // `remembered` reinsertion force that guess over the truth.
    if (tally.pending.length > 0 && plan.cameraIdentity?.source === "userSelected") {
      const evidence = plan.cameraEvidence ?? cameraEvidenceFor(
        plan.card,
        plan.items.filter((item) => item.classification === "new").map((item) => item.filename),
        context.state.imports.cameraExifHints[plan.sourcePath] ?? "",
      );
      rememberCameraMapping(context.state, projectId, evidence, plan.cameraIdentity.camera, context.clock.iso());
    }

    const controller = context.jobs.claimed("run_smart_import");
    if (!controller) {
      while (tally.pending.length > 0) stepProgress(context.state, tally, plan);
      return runReport(context.state, tally, plan, false, makeProxies);
    }

    const binding: JobBinding<void, RunOutcome, ImportRunReport> = {
      command: "run_smart_import",
      args: { sourcePath: plan.sourcePath },
      event: "import-progress",
      progress: () => stepProgress(context.state, tally, plan),
      result: (outcome) => runReport(
        context.state,
        tally,
        plan,
        Boolean(outcome?.cancelled) || controller.isCancelled,
        makeProxies,
        Boolean(outcome?.proxyCancelled),
      ),
    };
    return controller.bind(binding);
  },

  /**
   * The harness has no ffmpeg/ffprobe and no filesystem, so it stands in
   * with whatever the scenario already knows about a same-filename addition
   * — good enough to exercise the frontend's fetch-and-cache wiring without
   * claiming to be real media evidence (see `docs/integration-testing.md`).
   * A request naming a file no scenario declared degrades to `error`, the
   * same shape a real unreadable/missing-tool file would produce.
   */
  generate_import_preview: (args, { state }): ImportPreviewItem[] => {
    const requests = args.requests;
    if (!Array.isArray(requests)) throw new Error("generate_import_preview requires requests");
    const known = Object.values(state.imports.additions).flat();
    return requests.map((request) => {
      const path = typeof request?.path === "string" ? request.path : "";
      const filename = path.split("/").pop() ?? path;
      const asset = known.find((candidate) => candidate.filename === filename);
      if (!asset) {
        return { path, thumbnailPath: null, durationSeconds: null, capturedAt: null, error: "no preview available in the harness for this file" };
      }
      return {
        path,
        thumbnailPath: `/fake/import-preview/${request.fingerprint ?? filename}.jpg`,
        durationSeconds: asset.durationSeconds,
        capturedAt: asset.bestTime ?? asset.capturedAtRaw ?? null,
        error: null,
      };
    });
  },

  cancel_import: (_args, { jobs }) => {
    jobs.claimed("run_smart_import")?.requestCancel();
    return null;
  },

  list_import_sessions: (args, { state }) => {
    const projectId = requireString(args, "projectId");
    return Object.values(state.imports.sessions)
      .filter((entry) => entry.projectId === projectId)
      .map(({ files: _files, ...entry }) => entry);
  },

  import_session: (args, { state }): ImportSessionDetail => {
    requireString(args, "projectId");
    const sessionId = requireString(args, "sessionId");
    const session = state.imports.sessions[sessionId];
    if (!session) throw new Error(`Unknown import session: ${sessionId}`);
    return session;
  },

  import_rollback_preview: (args, { state }): ImportRollbackPreview => {
    requireString(args, "projectId");
    const sessionId = requireString(args, "sessionId");
    const session = state.imports.sessions[sessionId];
    if (!session) throw new Error(`Unknown import session: ${sessionId}`);
    const removable = session.files.filter((file) => file.classification === "new" && file.result === "completed" && file.assetId && state.assets[file.assetId]).length;
    return { sessionId, removableAssetCount: removable, alreadyRemovedAssetCount: session.rolledBackAssetCount, modifiedClips: [], affectedPhaseCount: 0, message: "Original files and archive copies stay untouched." };
  },

  rollback_import_membership: (args, { state }): ImportRollbackPreview => {
    requireString(args, "projectId");
    const sessionId = requireString(args, "sessionId");
    const session = state.imports.sessions[sessionId];
    if (!session) throw new Error(`Unknown import session: ${sessionId}`);
    for (const file of session.files) {
      if (file.classification === "new" && file.result === "completed" && file.assetId) delete state.assets[file.assetId];
    }
    state.assetOrder = state.assetOrder.filter((id) => Boolean(state.assets[id]));
    session.rolledBackAt ??= "2026-08-10T12:01:00.000Z";
    session.rolledBackAssetCount = session.imported;
    return { sessionId, removableAssetCount: 0, alreadyRemovedAssetCount: session.rolledBackAssetCount, modifiedClips: [], affectedPhaseCount: 0, message: "Original files and archive copies stay untouched." };
  },
};
