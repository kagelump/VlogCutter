import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

export function browserImageVersion(lock, workflow) {
  const versions = [...lock.matchAll(/^\s+'?@playwright\/test@([\d.]+)'?:/gm)].map((m) => m[1]);
  const image = workflow.match(/mcr\.microsoft\.com\/playwright:v([\d.]+)-noble@sha256:[a-f0-9]{64}\b/);
  if (versions.length === 0 || !image || versions.some((version) => version !== image[1])) {
    throw new Error("Official Playwright image must be digest-pinned and match every resolved @playwright/test version");
  }
  return image[1];
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const version = browserImageVersion(
    readFileSync(new URL("../pnpm-lock.yaml", import.meta.url), "utf8"),
    readFileSync(new URL("../.github/workflows/ci.yml", import.meta.url), "utf8"),
  );
  console.log(`Official Playwright image matches lockfile: ${version}`);
}
