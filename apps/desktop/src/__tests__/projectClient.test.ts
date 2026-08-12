/**
 * The native adapter behind the shared `ProjectClient` contract.
 *
 * Nothing here touches a real server: `api` is mocked, so what is under test is
 * only the translation — pairing state and a version handshake in, a
 * `ServerStatus` or a typed `ProjectClientError` out. Real transport, TLS, and
 * endpoint failover are the server tier's evidence, not this one's.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { ProjectClientError, SUPPORTED_PROTOCOL_MAJOR } from "@rcdvlog/project-client";
import type { ServerConnectionStatus, ServerVersionInfo } from "../types";

const serverConnectionStatus = vi.fn<() => Promise<ServerConnectionStatus>>();
const fetchServerInfo = vi.fn<(endpoint: string) => Promise<ServerVersionInfo>>();

vi.mock("../api", () => ({
  api: {
    serverConnectionStatus: () => serverConnectionStatus(),
    fetchServerInfo: (endpoint: string) => fetchServerInfo(endpoint),
  },
}));

const { projectClient } = await import("../projectClient");

function paired(overrides: Partial<ServerConnectionStatus> = {}): ServerConnectionStatus {
  return {
    paired: true,
    nodeId: "node_studio",
    endpoint: "https://studio.tail-scale.ts.net",
    label: "Studio",
    pairedAt: "2026-08-01T10:00:00.000Z",
    endpoints: null,
    ...overrides,
  };
}

function serverInfo(overrides: Partial<ServerVersionInfo> = {}): ServerVersionInfo {
  return {
    nodeId: "node_studio",
    applicationVersion: "0.4.1",
    protocolMajor: SUPPORTED_PROTOCOL_MAJOR,
    protocolMinor: 3,
    ...overrides,
  } as ServerVersionInfo;
}

describe("TauriProjectClient", () => {
  beforeEach(() => {
    serverConnectionStatus.mockReset();
    fetchServerInfo.mockReset();
  });

  it("reports a paired server's version and readiness", async () => {
    serverConnectionStatus.mockResolvedValue(paired());
    fetchServerInfo.mockResolvedValue(serverInfo());

    const status = await projectClient.getServerStatus();

    expect(fetchServerInfo).toHaveBeenCalledWith("https://studio.tail-scale.ts.net");
    expect(status.version.applicationVersion).toBe("0.4.1");
    expect(status.version.protocolVersion).toEqual({ major: SUPPORTED_PROTOCOL_MAJOR, minor: 3 });
    expect(status.version.nodeId).toBe("node_studio");
    expect(status.readiness.status).toBe("ready");
  });

  it("refuses to reach for a server this Mac was never paired with", async () => {
    serverConnectionStatus.mockResolvedValue(paired({ paired: false }));

    await expect(projectClient.getServerStatus()).rejects.toThrow(ProjectClientError);
    await expect(projectClient.getServerStatus()).rejects.toMatchObject({
      kind: "server_unavailable",
      code: "server_unpaired",
    });
    expect(fetchServerInfo).not.toHaveBeenCalled();
  });

  it("treats a pairing with no endpoint as unpaired rather than fetching nothing", async () => {
    serverConnectionStatus.mockResolvedValue(paired({ endpoint: null }));

    await expect(projectClient.getServerStatus()).rejects.toMatchObject({
      code: "server_unpaired",
    });
    expect(fetchServerInfo).not.toHaveBeenCalled();
  });

  it("rejects a server speaking an incompatible protocol major", async () => {
    serverConnectionStatus.mockResolvedValue(paired());
    fetchServerInfo.mockResolvedValue(serverInfo({ protocolMajor: SUPPORTED_PROTOCOL_MAJOR + 1 }));

    await expect(projectClient.getServerStatus()).rejects.toThrow(ProjectClientError);
  });
});
