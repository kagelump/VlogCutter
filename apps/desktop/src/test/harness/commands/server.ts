/**
 * Server pairing and sync status.
 *
 * Pairing state lives in settings exactly as it does in production, so the
 * status the app reads back after pairing is derived rather than declared.
 */

import type { ServerConnectionStatus, SyncStatus } from "../../../types";
import { buildSyncStatus } from "../builders";
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
};
