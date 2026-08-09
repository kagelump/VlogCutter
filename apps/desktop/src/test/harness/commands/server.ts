/**
 * Server pairing and sync status.
 *
 * Pairing state lives in settings exactly as it does in production, so the
 * status the app reads back after pairing is derived rather than declared.
 */

import type {
  BaselineImportResult,
  BaselinePreviewResult,
  PathAnalysis,
  RootMappingEntry,
  ServerConnectionStatus,
  ServerPairingResult,
  SyncStatus,
} from "../../../types";
import { buildSyncStatus } from "../builders";
import { projectAssets } from "../state";
import type { CommandTable } from "../registry";
import { requireString } from "../registry";

export const serverCommands: CommandTable = {
  server_connection_status: (_args, { state }): ServerConnectionStatus => ({
    paired: state.settings.pairedServerNodeId != null,
    nodeId: state.settings.pairedServerNodeId ?? null,
    endpoint: state.settings.pairedServerEndpoint ?? null,
    label: state.settings.pairedServerLabel ?? null,
    pairedAt: state.settings.pairedAt ?? null,
    endpoints: state.settings.pairedServerEndpoints ?? null,
  }),

  sync_status: (args, { state }): SyncStatus => {
    const projectId = requireString(args, "projectId");
    return state.sync[projectId] ?? buildSyncStatus(projectId);
  },

  sync_all_status: (_args, { state }) => Object.values(state.sync),

  project_pairing_status: (args, { state }) => {
    requireString(args, "projectId");
    return {
      paired: state.settings.pairedServerNodeId != null,
      serverNodeId: state.settings.pairedServerNodeId ?? null,
      pairedAt: state.settings.pairedAt ?? null,
    };
  },

  /** Reaching a server before pairing with it. A fault here is "cannot reach". */
  fetch_server_info: (args, { state }) => {
    requireString(args, "endpoint");
    return state.pairing.serverInfo;
  },

  pair_with_server: (args, { state, clock }): ServerPairingResult => {
    const endpoint = requireString(args, "endpoint");
    const pairingCode = requireString(args, "pairingCode");
    const label = requireString(args, "label");
    if (!state.pairing.acceptedCodes.includes(pairingCode)) {
      throw new Error("That pairing code is not valid or has expired");
    }
    const result: ServerPairingResult = {
      nodeId: state.pairing.serverInfo.nodeId,
      endpoint,
      label,
      pairedAt: clock.iso(),
    };
    // Pairing is only real once the settings the status reads from say so.
    state.settings.pairedServerNodeId = result.nodeId;
    state.settings.pairedServerEndpoint = endpoint;
    state.settings.pairedServerLabel = label;
    state.settings.pairedAt = result.pairedAt;
    state.settings.pairedServerEndpoints = [{ url: endpoint, kind: "lan", label }];
    return result;
  },

  unpair_server: (_args, { state }) => {
    state.settings.pairedServerNodeId = null;
    state.settings.pairedServerEndpoint = null;
    state.settings.pairedServerLabel = null;
    state.settings.pairedAt = null;
    state.settings.pairedServerEndpoints = null;
    return null;
  },

  /**
   * Sync is scriptable: without a claim it succeeds and marks the project
   * synced, so a CUJ about the happy path does not have to drive it.
   */
  sync_now: (args, { state, clock, jobs, emit }) => {
    const projectId = requireString(args, "projectId");
    const controller = jobs.claimed("sync_now");
    const settle = (): null => {
      const status = buildSyncStatus(projectId, {
        state: "synced",
        lastSyncAt: clock.iso(),
        activeEndpoint: state.settings.pairedServerEndpoint ?? null,
      });
      state.sync[projectId] = status;
      emit("sync-status-changed", status);
      return null;
    };
    if (!controller) return settle();
    return controller.bind({
      command: "sync_now",
      args: { projectId },
      event: "sync-status-changed",
      progress: (step) => {
        const status = buildSyncStatus(projectId, step as Partial<SyncStatus>);
        state.sync[projectId] = status;
        return status;
      },
      result: () => settle(),
    });
  },

  analyze_project_paths: (args, { state }): PathAnalysis => {
    const projectId = requireString(args, "projectId");
    const assets = projectAssets(state, projectId);
    const byPrefix = new Map<string, string[]>();
    for (const asset of assets) {
      const prefix = asset.path.slice(0, asset.path.lastIndexOf("/"));
      byPrefix.set(prefix, [...(byPrefix.get(prefix) ?? []), asset.filename]);
    }
    return {
      commonPrefixes: [...byPrefix.entries()].map(([prefix, filenames]) => ({
        prefix,
        assetCount: filenames.length,
        sampleAssets: filenames.slice(0, 3),
      })),
      totalAssets: assets.length,
      absolutePathCount: assets.length,
    };
  },

  propose_root_mappings: (args): RootMappingEntry[] => {
    const analysis = args.pathAnalysis as PathAnalysis | undefined;
    if (!analysis?.commonPrefixes) throw new Error("propose_root_mappings requires a path analysis");
    return analysis.commonPrefixes.map((prefix) => ({
      localPrefix: prefix.prefix,
      storageRootId: `root_${prefix.prefix.replace(/[^a-zA-Z0-9]/g, "-").toLowerCase().slice(0, 48)}`,
      kind: "new_logical_root",
      label: `${prefix.prefix} (${prefix.assetCount} assets)`,
    }));
  },

  preview_project_baseline: (args, { state }): BaselinePreviewResult => {
    const projectId = requireString(args, "projectId");
    const project = state.projects.find((candidate) => candidate.id === projectId);
    const assets = projectAssets(state, projectId);
    return {
      digest: state.pairing.baselineDigest,
      preview: {
        projectId,
        projectName: project?.name ?? projectId,
        assetCount: assets.length,
        operationCount: assets.filter((asset) => asset.labels.length > 0 || asset.notes).length,
        conflictCount: 0,
        linkedMediaCount: 0,
        proposedLibraryMatches: 0,
        rootMappingCount: 1,
        representationManifestCount: 0,
      },
      endpointUsed: state.settings.pairedServerEndpoint ?? "https://rcdvlog.local",
      serverNodeId: state.pairing.serverInfo.nodeId,
    };
  },

  confirm_baseline_import: (args, { state }): BaselineImportResult => {
    const projectId = requireString(args, "projectId");
    const expectedDigest = requireString(args, "expectedDigest");
    if (expectedDigest !== state.pairing.baselineDigest) {
      throw new Error("The project changed while you were reviewing the baseline. Preview it again.");
    }
    state.projects = state.projects.map((project) =>
      project.id === projectId
        ? { ...project, pairedServerNodeId: state.pairing.serverInfo.nodeId, pairedAt: state.settings.pairedAt ?? null }
        : project,
    );
    return { projectId, baselineDigest: expectedDigest, reusedExisting: false, state: "registered" };
  },

  abandon_baseline_attempt: () => null,
};
