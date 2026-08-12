/**
 * Stand-in frames for the browser preview.
 *
 * The desktop app resolves thumbnails through Tauri's asset protocol, which
 * outside Tauri points at nothing. That was harmless while the chronology
 * canvas was made of text, and is not now that the canvas *is* the frames: a
 * preview full of broken-image icons says nothing about whether the layout
 * works.
 *
 * So the preview draws its own. These are flat SVG cards derived from the
 * asset id, not decoded video, and they claim nothing about media handling —
 * see `docs/integration-testing.md` for what the preview is and is not.
 */

/** Distinct enough to tell two clips apart at a glance, dark enough to sit in the desk. */
const PALETTE = [
  ["#1d3a4a", "#2f6076"],
  ["#3a2c1c", "#6b4f2c"],
  ["#22323a", "#3d5c66"],
  ["#2e2038", "#523a63"],
  ["#1e3a2f", "#31624c"],
  ["#3a2028", "#6a3a48"],
];

function hashOf(value: string): number {
  let hash = 0;
  for (let index = 0; index < value.length; index += 1) {
    hash = (hash * 31 + value.charCodeAt(index)) >>> 0;
  }
  return hash;
}

/**
 * An `asset://`-shaped path in, a data URI out.
 *
 * The asset id is recovered from the path the scenario builder wrote
 * (`…/thumbnails/<assetId>/q1.v2.jpg`) so the same clip keeps the same frame
 * across re-renders and across the canvas, deck and queue.
 */
export function previewThumbnail(filePath: string): string {
  const assetId = filePath.match(/thumbnails\/([^/]+)\//)?.[1] ?? filePath;
  const hash = hashOf(assetId);
  const [dark, light] = PALETTE[hash % PALETTE.length];
  const angle = hash % 90;
  const label = assetId.replace(/^asset_/, "").slice(-6).toUpperCase();
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="320" height="180" viewBox="0 0 320 180">`
    + `<defs><linearGradient id="g" gradientTransform="rotate(${angle})">`
    + `<stop offset="0%" stop-color="${light}"/><stop offset="100%" stop-color="${dark}"/>`
    + `</linearGradient></defs>`
    + `<rect width="320" height="180" fill="url(#g)"/>`
    + `<circle cx="${40 + (hash % 240)}" cy="${30 + (hash % 90)}" r="26" fill="#ffffff" opacity="0.16"/>`
    + `<path d="M0 ${130 + (hash % 30)} L320 ${110 + (hash % 40)} L320 180 L0 180 Z" fill="#000000" opacity="0.34"/>`
    + `<text x="10" y="24" font-family="ui-monospace, Menlo, monospace" font-size="15"`
    + ` fill="#ffffff" opacity="0.62">${label}</text>`
    + `</svg>`;
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}

/** The `convertFileSrc` the preview installs: frames get one, everything else keeps the stub. */
export function previewConvertFileSrc(filePath: string, protocol = "asset"): string {
  if (/\/thumbnails\//.test(filePath)) return previewThumbnail(filePath);
  return `${protocol}://localhost/${encodeURIComponent(filePath)}`;
}
