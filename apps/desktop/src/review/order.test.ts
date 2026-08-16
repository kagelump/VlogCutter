import { describe, expect, it } from "vitest";
import type { Asset } from "../types";
import { indexOfAsset, moveSelection, nextSelection, orderAssets } from "./order";

function asset(overrides: Partial<Asset> & { id: string }): Asset {
  return {
    projectId: "project_1",
    path: `/media/${overrides.id}.mp4`,
    filename: `${overrides.id}.mp4`,
    fingerprint: `fp_${overrides.id}`,
    sizeBytes: 1_000_000,
    durationSeconds: 12,
    frameRate: 29.97,
    width: 1920,
    height: 1080,
    codec: "h264",
    camera: "Sony A7S III",
    capturedAtRaw: null,
    bestTime: null,
    offsetSeconds: 0,
    proxyPath: null,
    proxyRepresentation: null,
    metadataRotationDegrees: 0,
    defaultRotationDegrees: 0,
    rotationDegrees: 0,
    orientationProbed: true,
    originalAvailable: true,
    proxyAvailable: false,
    thumbnailPath: null,
    thumbnailQuality: 0,
    notes: "",
    title: "",
    description: "",
    labels: [],
    removed: false,
    mediaObjectId: null,
    mediaVerificationState: "unlinked",
    available: true,
    revision: 0,
    ...overrides,
  };
}

describe("orderAssets", () => {
  it("omits every asset whose removed is true, preserving relative order of the rest", () => {
    const assets = [
      asset({ id: "a" }),
      asset({ id: "b", removed: true }),
      asset({ id: "c" }),
      asset({ id: "d", removed: true }),
      asset({ id: "e" }),
    ];
    expect(orderAssets(assets, { removedVisible: false }).map((a) => a.id)).toEqual(["a", "c", "e"]);
  });

  it("shows only removed assets when removedVisible is true", () => {
    const assets = [
      asset({ id: "a" }),
      asset({ id: "b", removed: true }),
      asset({ id: "c", removed: true }),
    ];
    expect(orderAssets(assets, { removedVisible: true }).map((a) => a.id)).toEqual(["b", "c"]);
  });

  it("is stable: calling twice on the same input returns the same order", () => {
    const assets = [asset({ id: "a" }), asset({ id: "b" }), asset({ id: "c", removed: true })];
    const first = orderAssets(assets, { removedVisible: false });
    const second = orderAssets(assets, { removedVisible: false });
    expect(first.map((a) => a.id)).toEqual(second.map((a) => a.id));
  });

  it("never lets two distinct assets collapse into one entry (total, no dedup by any implicit key)", () => {
    // Two assets with identical bestTime and near-identical filenames (differ only
    // by a trailing counter) must both survive filtering independently.
    const assets = [
      asset({ id: "dup-1", bestTime: "2026-01-01T00:00:00Z", filename: "IMG_0001.mp4" }),
      asset({ id: "dup-2", bestTime: "2026-01-01T00:00:00Z", filename: "IMG_0002.mp4" }),
    ];
    const ordered = orderAssets(assets, { removedVisible: false });
    expect(ordered).toHaveLength(2);
    expect(new Set(ordered.map((a) => a.id)).size).toBe(2);
  });

  it("is total: every asset appears in exactly one of the two removedVisible views", () => {
    const assets = [asset({ id: "a" }), asset({ id: "b", removed: true }), asset({ id: "c" })];
    const active = orderAssets(assets, { removedVisible: false });
    const removed = orderAssets(assets, { removedVisible: true });
    expect(active.length + removed.length).toBe(assets.length);
  });
});

describe("indexOfAsset", () => {
  it("returns the index of a present asset", () => {
    const ordered = [asset({ id: "a" }), asset({ id: "b" }), asset({ id: "c" })];
    expect(indexOfAsset(ordered, "b")).toBe(1);
  });

  it("returns -1 when the asset id is absent", () => {
    const ordered = [asset({ id: "a" }), asset({ id: "b" })];
    expect(indexOfAsset(ordered, "missing")).toBe(-1);
  });

  it("returns -1 when assetId is null", () => {
    const ordered = [asset({ id: "a" })];
    expect(indexOfAsset(ordered, null)).toBe(-1);
  });
});

describe("moveSelection", () => {
  const ordered = [asset({ id: "first" }), asset({ id: "mid" }), asset({ id: "last" })];

  it("does not wrap past the start: moveSelection(ordered, firstId, -1) returns firstId", () => {
    expect(moveSelection(ordered, "first", -1)).toBe("first");
  });

  it("does not wrap past the end: moveSelection(ordered, lastId, +1) returns lastId", () => {
    expect(moveSelection(ordered, "last", 1)).toBe("last");
  });

  it("moves forward one position from the middle", () => {
    expect(moveSelection(ordered, "mid", 1)).toBe("last");
  });

  it("moves backward one position from the middle", () => {
    expect(moveSelection(ordered, "mid", -1)).toBe("first");
  });

  it("moveSelection(ordered, null, +1) returns the first asset's id", () => {
    expect(moveSelection(ordered, null, 1)).toBe("first");
  });

  it("returns the current id unchanged when ordered is empty", () => {
    expect(moveSelection([], "anything", 1)).toBe("anything");
    expect(moveSelection([], null, 1)).toBeNull();
  });

  it("clamps a large positive delta to the last item", () => {
    expect(moveSelection(ordered, "first", 10)).toBe("last");
  });

  it("clamps a large negative delta to the first item", () => {
    expect(moveSelection(ordered, "last", -10)).toBe("first");
  });
});

describe("nextSelection", () => {
  it("returns the same id when the asset survives a reorder", () => {
    const before = [asset({ id: "a" }), asset({ id: "b" }), asset({ id: "c" }), asset({ id: "d" })];
    const selected = before[2].id;
    // Simulate a reorder: same assets, different sequence.
    const after = [before[3], before[1], before[2], before[0]];
    expect(nextSelection(after, selected)).toBe(selected);
  });

  it("falls back to the nearest surviving neighbour when the asset does not survive", () => {
    const after = [asset({ id: "x" }), asset({ id: "y" }), asset({ id: "z" })];
    expect(nextSelection(after, "gone")).toBe("x");
  });

  it("returns null when nothing survives", () => {
    expect(nextSelection([], "gone")).toBeNull();
  });

  it("returns null when nothing survives and there was no previous selection", () => {
    expect(nextSelection([], null)).toBeNull();
  });
});
