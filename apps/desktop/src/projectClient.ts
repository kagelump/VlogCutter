import {
  ProjectClientError,
  assertProtocolCompatible,
  type ProjectClient,
  type ServerStatus,
} from "@rcdvlog/project-client";
import { api } from "./api";

/**
 * Native adapter for the operations shared with the web surface.
 * Existing desktop features continue through `api`; this seam lets shared
 * review primitives depend on a transport-neutral client.
 */
export class TauriProjectClient implements ProjectClient {
  async getServerStatus(): Promise<ServerStatus> {
    const connection = await api.serverConnectionStatus();
    if (!connection.paired || !connection.endpoint) {
      throw new ProjectClientError(
        "server_unavailable",
        "Pair this Mac with a server before checking its status.",
        "server_unpaired",
      );
    }
    const info = await api.fetchServerInfo(connection.endpoint);
    const version = {
      requestId: "tauri",
      applicationVersion: info.applicationVersion,
      protocolVersion: { major: info.protocolMajor, minor: info.protocolMinor },
      controlSchemaVersion: 0,
      nodeId: info.nodeId,
    };
    assertProtocolCompatible(version);
    return {
      version,
      readiness: {
        requestId: "tauri",
        status: "ready",
        checks: [],
      },
    };
  }
}

export const projectClient: ProjectClient = new TauriProjectClient();
