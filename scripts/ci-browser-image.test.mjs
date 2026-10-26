import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import { browserImageVersion } from "./ci-browser-image.mjs";
const image = `image: mcr.microsoft.com/playwright:v1.61.1-noble@sha256:${"a".repeat(64)}`;
const lock = "  '@playwright/test@1.61.1':\n";
test("the actual workflow image matches every resolved lockfile version", () => {
  assert.ok(browserImageVersion(readFileSync(new URL("../pnpm-lock.yaml", import.meta.url), "utf8"), readFileSync(new URL("../.github/workflows/ci.yml", import.meta.url), "utf8")));
});
test("image verification rejects version skew, multiple resolutions and mutable images", () => {
  assert.equal(browserImageVersion(lock, image), "1.61.1");
  for (const [badLock, badImage] of [
    ["", image], [lock, ""], [lock.replace("1.61.1", "1.62.0"), image],
    [lock + "  '@playwright/test@1.62.0':\n", image],
    [lock, "image: mcr.microsoft.com/playwright:v1.61.1-noble"],
    [lock, image.replace("sha256:", "sha256:z")],
  ]) assert.throws(() => browserImageVersion(badLock, badImage), /digest-pinned and match/);
});
