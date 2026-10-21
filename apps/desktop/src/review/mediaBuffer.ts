/**
 * The ±1 media window every playback surface mounts.
 *
 * Review builds it over the queue order; the chronology desk builds it over the
 * solver's order. Both need the same four decisions made the same way — which
 * representation to serve, how much to rotate it given that choice, what aspect
 * the frame ends up at, and which poster agrees with the pixels — so those
 * decisions live here rather than twice.
 *
 * Mounting the neighbours is what makes stepping feel like scrubbing one long
 * take instead of opening files: J/K and the compare deck's arrows pay no load.
 */

import type { Asset, MediaOrigin } from "../types";
import { isRawOrientationProxy, renderedRotation } from "./rotation";

/** One clip mounted in a stage: the active one and its two neighbours. */
export interface BufferedMedia {
  asset: Asset;
  /** -1, 0, or 1 relative to the active clip. */
  offset: number;
  active: boolean;
  /** Null is filtered out before this type is handed to a stage. */
  url: string | null;
  /**
   * Whether the media behind `url` leaves the pixels raw and carries rotation
   * as data. Resolved here, because it depends on which of the proxy and the
   * original the URL points at.
   */
  mediaIsRawOriented: boolean;
  /** Degrees to turn the frame, already resolved against the representation. */
  rotation: number;
  displayAspect: number;
  posterUrl: string | null;
}

/** Which bytes the origin should serve, or null when neither is reachable. */
function representationOf(asset: Asset): "proxy" | "original" | "camera_proxy" | null {
  if (asset.proxyAvailable && asset.proxyPath) return "proxy";
  if (asset.originalAvailable) return "original";
  return asset.cameraProxyAvailable ? "camera_proxy" : null;
}

/**
 * The loopback media URL for a clip.
 *
 * No path is ever sent: the origin resolves the id through the catalog, and
 * refuses the request outright without the session token. `rep` rides along so
 * the origin serves exactly the representation the caller resolved rotation
 * against.
 */
export function mediaUrlFor(asset: Asset, origin: MediaOrigin | null): string | null {
  const representation = representationOf(asset);
  if (!representation || !origin) return null;
  const query = new URLSearchParams({ rep: representation, t: origin.token });
  return `${origin.baseUrl}/media/${encodeURIComponent(asset.id)}?${query}`;
}

/** The rotation and aspect a clip's frame is drawn at, given its representation. */
export function frameGeometry(asset: Asset) {
  const usingProxy = Boolean(asset.proxyAvailable && asset.proxyPath);
  const mediaIsRawOriented = usingProxy && isRawOrientationProxy(asset);
  const quarterTurned = asset.rotationDegrees % 180 !== 0;
  return {
    mediaIsRawOriented,
    rotation: renderedRotation(asset, mediaIsRawOriented),
    // `|| 16 / 9` also catches an unmeasured clip, whose zero dimensions would
    // otherwise divide out to NaN and collapse the stage.
    displayAspect: (quarterTurned ? asset.height / asset.width : asset.width / asset.height) || 16 / 9,
  };
}

/**
 * Mounts `activeId` and its immediate neighbours from `ordered`.
 *
 * `ordered` is whatever order the calling surface is presenting — the filtered
 * review queue, or the solver's chronology. Clips with no playable URL are
 * dropped rather than mounted empty, which is also what makes the browser
 * preview (no origin at all) return a clean empty buffer.
 *
 * @param thumbnailUrlFor Resolves a poster. Injected because it goes through
 *   Tauri's asset protocol, which does not exist under test.
 */
export function buildMediaBuffer(
  ordered: Asset[],
  activeId: string | null,
  origin: MediaOrigin | null,
  thumbnailUrlFor: (asset: Asset) => string | null = () => null,
): BufferedMedia[] {
  if (!activeId) return [];
  const activeIndex = ordered.findIndex((asset) => asset.id === activeId);
  if (activeIndex < 0) return [];

  return [-1, 0, 1]
    .map((offset) => ({ asset: ordered[activeIndex + offset], offset }))
    .filter((entry): entry is { asset: Asset; offset: number } =>
      Boolean(entry.asset?.orientationProbed && entry.asset.available))
    .map(({ asset, offset }) => ({
      asset,
      offset,
      active: offset === 0,
      url: mediaUrlFor(asset, origin),
      posterUrl: thumbnailUrlFor(asset),
      ...frameGeometry(asset),
    }))
    .filter((entry) => Boolean(entry.url));
}
