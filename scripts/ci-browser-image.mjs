import { readFileSync } from "node:fs";

export function browserImageVersion(lock, workflow) {
  const versions = [...lock.matchAll(/^\s+'?@playwright\/test@([\d.]+)'?:/gm)].map((m) => m[1]);
  // Every job that runs in the official image (the desktop browser tier and the
  // real-server web tier) must be pinned to the same digest-verified version.
  const references = [...workflow.matchAll(/mcr\.microsoft\.com\/playwright\S*/g)].map((m) => m[0]);
  const images = references.map((reference) => reference.match(/^mcr\.microsoft\.com\/playwright:v([\d.]+)-noble@sha256:[a-f0-9]{64}$/));
  if (versions.length === 0 || images.length === 0 || images.some((image) => !image)
    || versions.some((version) => images.some((image) => image[1] !== version))) {
    throw new Error("Official Playwright image must be digest-pinned and match every resolved @playwright/test version");
  }
  return images[0][1];
}
// import.meta.main survives symlinked paths (macOS /var -> /private/var).
if (import.meta.main) {
  const version = browserImageVersion(
    readFileSync(new URL("../pnpm-lock.yaml", import.meta.url), "utf8"),
    readFileSync(new URL("../.github/workflows/ci.yml", import.meta.url), "utf8"),
  );
  console.log(`Official Playwright image matches lockfile: ${version}`);
}
