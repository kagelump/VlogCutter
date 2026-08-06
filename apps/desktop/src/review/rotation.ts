import type { Asset } from "../types";

/**
 * How much to turn a frame before showing it.
 *
 * Two representations exist for both proxies and thumbnails. The older one
 * bakes the camera's metadata rotation into the pixels; the newer one leaves
 * the pixels raw and carries the rotation as data. Rendering therefore cannot
 * ask "how is this clip rotated" alone — it has to ask that of a specific
 * representation, which is what `renderedRotation` takes its second argument
 * for.
 */

/** Normalises to 0-359, for negative additions as well as positive. */
export function rotationAfter(rotationDegrees: number, addition: number) {
  return ((rotationDegrees + addition) % 360 + 360) % 360;
}

/** v3 proxies keep the pixels raw and carry rotation as data. */
export function isRawOrientationProxy(asset: Asset) {
  return asset.proxyRepresentation === "raw_v3";
}

/** The thumbnail equivalent, recorded in the filename rather than a column. */
export function isRawOrientationThumbnail(asset: Asset) {
  return Boolean(asset.thumbnailPath?.endsWith(".v2.jpg"));
}

/**
 * The rotation to apply to a rendered frame. Baked representations have the
 * metadata rotation already in their pixels, so it is subtracted back out.
 */
export function renderedRotation(asset: Asset, representationIsRaw: boolean) {
  return rotationAfter(
    asset.rotationDegrees,
    representationIsRaw ? 0 : -asset.metadataRotationDegrees,
  );
}
