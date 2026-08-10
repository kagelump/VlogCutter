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
  CardCandidate,
  ImportPreflight,
  ImportProgress,
  ImportRunReport,
  PreflightItem,
} from "../../../types";
import { buildArchiveRoot } from "../builders";
import type { JobBinding } from "../jobs";
import { projectAssets, putAsset, type DesktopHarnessState } from "../state";
import type { CommandTable, HarnessContext } from "../registry";
import { requireBoolean, requireString } from "../registry";

/**
 * Build the plan for a source, from what the scenario said is on the card and
 * what the project already holds. Pure: it reads state and writes none.
 */
function preflight(
  state: DesktopHarnessState,
  projectId: string,
  sourcePath: string,
  archiveRootId: string | null,
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

  const items: PreflightItem[] = additions.map((asset) => ({
    path: `${sourcePath}/${asset.filename}`,
    relativePath: asset.filename,
    filename: asset.filename,
    sizeBytes: asset.sizeBytes,
    classification: state.assets[asset.id] ? "knownVerified" : "new",
    assetId: state.assets[asset.id] ? asset.id : null,
    error: "",
  }));

  return {
    sourcePath,
    card,
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
    items,
  };
}

interface RunTally {
  imported: string[];
  pending: Asset[];
  bytesCopied: number;
}

function stepProgress(state: DesktopHarnessState, tally: RunTally, plan: ImportPreflight): ImportProgress {
  const next = tally.pending.shift();
  if (!next) throw new Error("The import has no pending clips left to report");
  putAsset(state, next);
  tally.imported.push(next.id);
  tally.bytesCopied += next.sizeBytes;
  return {
    stage: "copying",
    completed: tally.imported.length,
    total: plan.newClips,
    currentFile: next.filename,
    completedBytes: tally.bytesCopied,
    totalBytes: plan.destination.bytesToCopy,
  };
}

function runReport(state: DesktopHarnessState, tally: RunTally, plan: ImportPreflight, cancelled: boolean, makeProxies: boolean): ImportRunReport {
  return {
    outcome: {
      imported: tally.imported.length,
      relinked: 0,
      skipped: plan.knownClips,
      copied: tally.imported.length,
      bytesCopied: tally.bytesCopied,
      cancelled,
      failures: [],
    },
    proxies: makeProxies
      ? { generated: tally.imported.length, skipped: 0, cancelled, failures: [] }
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
    return preflight(state, projectId, sourcePath, archiveRootId);
  },

  run_smart_import: (args, context: HarnessContext): ImportRunReport | Promise<unknown> => {
    requireString(args, "projectId");
    const plan = args.plan as ImportPreflight | undefined;
    if (!plan?.sourcePath) throw new Error("run_smart_import requires the plan it previewed");
    requireBoolean(args, "verifyCopies");
    const makeProxies = requireBoolean(args, "makeProxies");
    if (!plan.destination.ready) throw new Error(plan.destination.blockedReason || "This destination is not ready");

    context.state.imports.lastRunSourcePath = plan.sourcePath;
    const additions = context.state.imports.additions[plan.sourcePath] ?? [];
    const tally: RunTally = {
      imported: [],
      // Exactly the clips the plan classified as new, and no others.
      pending: additions.filter((asset) => !context.state.assets[asset.id]),
      bytesCopied: 0,
    };

    const controller = context.jobs.claimed("run_smart_import");
    if (!controller) {
      while (tally.pending.length > 0) stepProgress(context.state, tally, plan);
      return runReport(context.state, tally, plan, false, makeProxies);
    }

    const binding: JobBinding<void, { cancelled?: boolean }, ImportRunReport> = {
      command: "run_smart_import",
      args: { sourcePath: plan.sourcePath },
      event: "import-progress",
      progress: () => stepProgress(context.state, tally, plan),
      result: (outcome) => runReport(context.state, tally, plan, Boolean(outcome?.cancelled) || controller.isCancelled, makeProxies),
    };
    return controller.bind(binding);
  },

  cancel_import: (_args, { jobs }) => {
    jobs.claimed("run_smart_import")?.requestCancel();
    return null;
  },
};
