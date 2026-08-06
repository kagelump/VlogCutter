import { describe, expect, it } from "vitest";
import type { Asset } from "../types";
import {
  isRawOrientationProxy,
  isRawOrientationThumbnail,
  portraitRotation,
  renderedRotation,
  rotationAfter,
} from "./rotation";

function asset(overrides: Partial<Asset> = {}): Asset {
  return {
    rotationDegrees: 0,
    defaultRotationDegrees: 0,
    metadataRotationDegrees: 0,
    proxyRepresentation: null,
    thumbnailPath: null,
    ...overrides,
  } as Asset;
}

describe("rotationAfter", () => {
  it("stays inside 0-359 in both directions", () => {
    expect(rotationAfter(0, 90)).toBe(90);
    expect(rotationAfter(270, 90)).toBe(0);
    expect(rotationAfter(0, -90)).toBe(270);
    expect(rotationAfter(0, -450)).toBe(270);
  });
});

describe("portraitRotation", () => {
  it("is a quarter turn back from the asset's default", () => {
    expect(portraitRotation(asset({ defaultRotationDegrees: 0 }))).toBe(270);
    expect(portraitRotation(asset({ defaultRotationDegrees: 90 }))).toBe(0);
  });
});

describe("representation probes", () => {
  it("recognises a raw-orientation proxy only at v3", () => {
    expect(isRawOrientationProxy(asset({ proxyRepresentation: "raw_v3" }))).toBe(true);
    expect(isRawOrientationProxy(asset({ proxyRepresentation: "metadata_baked_v1" }))).toBe(false);
    expect(isRawOrientationProxy(asset({ proxyRepresentation: "metadata_tagged_raw_v2" }))).toBe(false);
    expect(isRawOrientationProxy(asset())).toBe(false);
  });

  it("recognises a raw-orientation thumbnail by its .v2.jpg suffix", () => {
    expect(isRawOrientationThumbnail(asset({ thumbnailPath: "/t/a.v2.jpg" }))).toBe(true);
    expect(isRawOrientationThumbnail(asset({ thumbnailPath: "/t/a.jpg" }))).toBe(false);
    expect(isRawOrientationThumbnail(asset())).toBe(false);
  });
});

describe("renderedRotation", () => {
  it("applies the asset rotation as-is to raw media", () => {
    expect(renderedRotation(asset({ rotationDegrees: 90, metadataRotationDegrees: 90 }), true)).toBe(90);
  });

  it("backs out metadata rotation already baked into the pixels", () => {
    expect(renderedRotation(asset({ rotationDegrees: 90, metadataRotationDegrees: 90 }), false)).toBe(0);
    expect(renderedRotation(asset({ rotationDegrees: 0, metadataRotationDegrees: 90 }), false)).toBe(270);
  });
});
