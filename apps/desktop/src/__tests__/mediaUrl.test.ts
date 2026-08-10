import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Asset, DesktopSettings, MediaOrigin } from "../types";

const invoke = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args?: unknown) => invoke(command, args),
  convertFileSrc: (path: string) => `asset://localhost/${encodeURIComponent(path)}`,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

const origin: MediaOrigin = { baseUrl: "http://127.0.0.1:49512", token: "s3cret-token_value" };

const settings = (mediaOrigin: MediaOrigin | null): DesktopSettings => ({
  projectsRoot: "/Users/demo/Movies/VideoProjects",
  nodeId: "node_desktop_test",
  autoplayMode: "proxyAvailable",
  lastCatalogPath: null,
  lastProjectId: null,
  lastAssetId: null,
  lastContextUpdatedAt: null,
  makeProxiesOnImport: true,
  lastImportArchiveRootId: null,
  mediaOrigin,
});

function asset(overrides: Partial<Asset> = {}): Asset {
  return {
    id: "asset_1", projectId: "p", path: "/Volumes/Travel/C0163.MP4", filename: "C0163.MP4",
    fingerprint: "a", sizeBytes: 1_000_000, durationSeconds: 12, frameRate: 29.97,
    width: 3840, height: 2160, codec: "hevc", camera: "Sony", capturedAtRaw: null,
    bestTime: null, offsetSeconds: 0, proxyPath: null, proxyRepresentation: null,
    metadataRotationDegrees: 0, defaultRotationDegrees: 0, rotationDegrees: 0,
    orientationProbed: true, originalAvailable: true, proxyAvailable: false,
    thumbnailPath: null, thumbnailQuality: 0, notes: "", title: "", description: "",
    labels: [], removed: false, mediaObjectId: null, mediaVerificationState: "unlinked",
    available: true,
    ...overrides,
  } as unknown as Asset;
}

/**
 * A fresh module instance, because the media origin is module-level state.
 *
 * There is no "outside Tauri" case to load any more: the browser preview
 * installs the shared fake backend, so these adapters have one path everywhere.
 * A preview clip has no URL because its settings carry no origin — the state
 * covered below by "has no URL to give until the origin has been handed over" —
 * rather than because `api.ts` branched on the environment.
 */
async function loadApi() {
  vi.resetModules();
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
  return (await import("../api")).api;
}

beforeEach(() => {
  invoke.mockReset();
});

afterEach(() => {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  delete (window as unknown as Record<string, unknown>).__RCDVLOG_MEDIA_ORIGIN__;
});

/** The dev-only handle the eval bridge reads. */
const devHandle = () => (window as unknown as Record<string, unknown>).__RCDVLOG_MEDIA_ORIGIN__;

describe("api.mediaUrl", () => {
  it("addresses the loopback origin by asset id, never by path", async () => {
    const api = await loadApi();
    invoke.mockResolvedValueOnce(settings(origin));
    await api.desktopSettings();

    const url = new URL(api.mediaUrl(asset()) as string);
    expect(url.origin).toBe(origin.baseUrl);
    expect(url.pathname).toBe("/media/asset_1");
    expect(url.searchParams.get("t")).toBe(origin.token);
    // Requirement 3: no filesystem path is ever sent to the origin.
    expect(url.search).not.toContain("Volumes");
    expect(decodeURIComponent(url.toString())).not.toContain("/Volumes/Travel/C0163.MP4");
  });

  it("keeps the proxy-versus-original choice the review queue already makes", async () => {
    const api = await loadApi();
    invoke.mockResolvedValueOnce(settings(origin));
    await api.desktopSettings();

    const withProxy = asset({ proxyPath: "/cache/proxies/asset_1.mp4", proxyAvailable: true });
    expect(new URL(api.mediaUrl(withProxy) as string).searchParams.get("rep")).toBe("proxy");

    // A recorded proxy that is not on disk falls back exactly as before.
    const missingProxy = asset({ proxyPath: "/cache/proxies/asset_1.mp4", proxyAvailable: false });
    expect(new URL(api.mediaUrl(missingProxy) as string).searchParams.get("rep")).toBe("original");

    // Nothing available is still nothing: the stage shows its offline slate.
    expect(api.mediaUrl(asset({ originalAvailable: false, available: false }))).toBeNull();
  });

  it("has no URL to give until the origin has been handed over", async () => {
    const api = await loadApi();
    expect(api.mediaUrl(asset())).toBeNull();

    // An origin that failed to bind is reported as absent rather than as a URL
    // that would never answer.
    invoke.mockResolvedValueOnce(settings(null));
    await api.desktopSettings();
    expect(api.mediaUrl(asset())).toBeNull();
  });

  it("rebuilds URLs from a restarted origin", async () => {
    const api = await loadApi();
    invoke.mockResolvedValueOnce(settings(origin));
    await api.desktopSettings();
    expect(api.mediaUrl(asset())).toContain(origin.token);

    const restarted: MediaOrigin = { baseUrl: "http://127.0.0.1:52001", token: "second-token" };
    invoke.mockResolvedValueOnce(restarted);
    expect(await api.recoverMediaOrigin()).toEqual(restarted);
    expect(invoke).toHaveBeenLastCalledWith("recover_media_origin", undefined);

    const url = new URL(api.mediaUrl(asset()) as string);
    expect(url.origin).toBe(restarted.baseUrl);
    expect(url.searchParams.get("t")).toBe(restarted.token);
  });

  it("drops every URL when the origin could not be restarted", async () => {
    const api = await loadApi();
    invoke.mockResolvedValueOnce(settings(origin));
    await api.desktopSettings();

    invoke.mockResolvedValueOnce(null);
    expect(await api.recoverMediaOrigin()).toBeNull();
    expect(api.mediaUrl(asset())).toBeNull();
  });

  it("publishes the dev handle only when the app offers a debug URL", async () => {
    const api = await loadApi();
    invoke.mockResolvedValueOnce(settings(origin));
    await api.desktopSettings();
    // A release app never sets `debugUrl`, so the eval bridge sees nothing.
    expect(devHandle()).toBeUndefined();

    const debugging: MediaOrigin = { ...origin, debugUrl: `${origin.baseUrl}/debug/media?t=${origin.token}` };
    invoke.mockResolvedValueOnce(settings(debugging));
    await api.desktopSettings();
    expect(devHandle()).toEqual(debugging);

    // And it goes away again if a restarted origin has diagnostics off.
    invoke.mockResolvedValueOnce(origin);
    await api.recoverMediaOrigin();
    expect(devHandle()).toBeUndefined();
  });

  it("still serves thumbnails through the asset protocol", async () => {
    // Images are unaffected by the WKWebView limitation, so posters keep the
    // transport they already had; this task moves video only.
    const api = await loadApi();
    expect(api.thumbnailUrl(asset({ thumbnailPath: "/cache/thumbnails/asset_1/q1.v2.jpg" })))
      .toBe("asset://localhost/%2Fcache%2Fthumbnails%2Fasset_1%2Fq1.v2.jpg");
    expect(api.thumbnailUrl(asset())).toBeNull();
  });
});
