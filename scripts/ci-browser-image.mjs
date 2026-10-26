import { readFileSync } from "node:fs";
const lock = readFileSync(new URL("../pnpm-lock.yaml", import.meta.url), "utf8");
const workflow = readFileSync(new URL("../.github/workflows/ci.yml", import.meta.url), "utf8");
const versions = [...lock.matchAll(/^\s+'?@playwright\/test@([\d.]+)'?:/gm)].map((m) => m[1]);
const image = workflow.match(/mcr\.microsoft\.com\/playwright:v([\d.]+)-noble@sha256:[a-f0-9]{64}/);
if (versions.length === 0 || !image || versions.some((version) => version !== image[1])) {
  throw new Error("Official Playwright image must be digest-pinned and match every resolved @playwright/test version");
}
console.log(`Official Playwright image matches lockfile: ${image[1]}`);
