import { describe, expect, it } from "vitest";
import { buildCameraAssets } from "../test/harness/builders";
import type { Asset } from "../types";
import { buildMediaBuffer } from "./mediaBuffer";

const ORIGIN = { baseUrl: "http://127.0.0.1:7788", token: "tok" };

function assets(overrides: Partial<Asset> = {}): Asset[] {
  return buildCameraAssets({
    projectId: "project_test",
    camera: "Sony A7C II",
    spec: { clips: 5, withProxy: true },
  }).map((asset) => ({ ...asset, ...overrides }));
}

describe("buildMediaBuffer", () => {
  it("mounts the active clip and its two neighbours", () => {
    const buffer = buildMediaBuffer(assets(), "asset_sony_a7c_ii_003", ORIGIN);
    expect(buffer.map((entry) => [entry.asset.id, entry.offset, entry.active])).toEqual([
      ["asset_sony_a7c_ii_002", -1, false],
      ["asset_sony_a7c_ii_003", 0, true],
      ["asset_sony_a7c_ii_004", 1, false],
    ]);
  });

  it("clips the window at both ends of the order", () => {
    const ordered = assets();
    expect(buildMediaBuffer(ordered, ordered[0].id, ORIGIN).map((entry) => entry.offset))
      .toEqual([0, 1]);
    expect(buildMediaBuffer(ordered, ordered[4].id, ORIGIN).map((entry) => entry.offset))
      .toEqual([-1, 0]);
  });

  it("returns nothing when the active clip is absent or unset", () => {
    expect(buildMediaBuffer(assets(), "asset_missing", ORIGIN)).toEqual([]);
    expect(buildMediaBuffer(assets(), null, ORIGIN)).toEqual([]);
  });

  it("returns nothing when no media origin has been bound", () => {
    // The browser preview is in exactly this state, and so is the desktop app
    // when the loopback origin failed to bind: no URL means no playable entry.
    expect(buildMediaBuffer(assets(), "asset_sony_a7c_ii_003", null)).toEqual([]);
  });

  it("carries the session token and representation in every URL", () => {
    const [entry] = buildMediaBuffer(assets(), "asset_sony_a7c_ii_001", ORIGIN);
    expect(entry.url).toBe(
      "http://127.0.0.1:7788/media/asset_sony_a7c_ii_001?rep=proxy&t=tok",
    );
  });

  it("falls back to the original when the proxy is missing", () => {
    const ordered = assets({ proxyAvailable: false, proxyPath: null, proxyRepresentation: null });
    const [entry] = buildMediaBuffer(ordered, ordered[0].id, ORIGIN);
    expect(entry.url).toContain("rep=original");
    expect(entry.mediaIsRawOriented).toBe(false);
  });

  it("drops clips whose original and proxy are both gone", () => {
    const ordered = assets({ proxyAvailable: false, proxyPath: null, originalAvailable: false, available: false });
    expect(buildMediaBuffer(ordered, ordered[0].id, ORIGIN)).toEqual([]);
  });

  it("drops clips whose orientation has not been probed yet", () => {
    // The stage shows a "reading orientation" slate for these rather than a
    // frame, so mounting them would paint the wrong way up.
    const ordered = assets({ orientationProbed: false });
    expect(buildMediaBuffer(ordered, ordered[0].id, ORIGIN)).toEqual([]);
  });

  it("resolves rotation against the representation actually being served", () => {
    // A raw_v3 proxy keeps the pixels unrotated and carries rotation as data,
    // so the full user rotation is applied. A baked original has the camera's
    // metadata rotation already in its pixels, so it is subtracted back out.
    const [viaProxy] = buildMediaBuffer(
      assets({ metadataRotationDegrees: 90, rotationDegrees: 90 }),
      "asset_sony_a7c_ii_001",
      ORIGIN,
    );
    expect(viaProxy.mediaIsRawOriented).toBe(true);
    expect(viaProxy.rotation).toBe(90);

    const [viaOriginal] = buildMediaBuffer(
      assets({
        metadataRotationDegrees: 90,
        rotationDegrees: 90,
        proxyAvailable: false,
        proxyPath: null,
        proxyRepresentation: null,
      }),
      "asset_sony_a7c_ii_001",
      ORIGIN,
    );
    expect(viaOriginal.rotation).toBe(0);
  });

  it("reports the aspect the frame is displayed at, not the stored one", () => {
    const [landscape] = buildMediaBuffer(assets(), "asset_sony_a7c_ii_001", ORIGIN);
    expect(landscape.displayAspect).toBeCloseTo(3840 / 2160);

    const [quarterTurned] = buildMediaBuffer(
      assets({ rotationDegrees: 90 }),
      "asset_sony_a7c_ii_001",
      ORIGIN,
    );
    expect(quarterTurned.displayAspect).toBeCloseTo(2160 / 3840);
  });

  it("falls back to 16:9 when the clip was never measured", () => {
    const [entry] = buildMediaBuffer(assets({ width: 0, height: 0 }), "asset_sony_a7c_ii_001", ORIGIN);
    expect(entry.displayAspect).toBeCloseTo(16 / 9);
  });
});
