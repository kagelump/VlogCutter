import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const ci = readFileSync(resolve(root, ".github/workflows/ci.yml"), "utf8");
const nextestConfig = readFileSync(resolve(root, ".config/nextest.toml"), "utf8");

const linuxRust = ci.slice(ci.indexOf("\n  linux-rust:"), ci.indexOf("\n  frontend:"));

test("nextest is pinned to an exact version and installed through the pinned action", () => {
  assert.match(ci, /^  NEXTEST_VERSION: "\d+\.\d+\.\d+"$/m);
  assert.match(linuxRust, /uses: taiki-e\/install-action@[0-9a-f]{40} # v/);
  assert.match(linuxRust, /tool: nextest@\$\{\{ env\.NEXTEST_VERSION \}\}/);
  assert.match(linuxRust, /name: Install cargo-nextest[^\n]*\n[^\n]*\n\s+timeout-minutes: \d+/, "install step is bounded");
});

test("every shard 1..N exists exactly once and the partition count matches", () => {
  const shards = [...linuxRust.matchAll(/^\s+shard: (\d+)$/gm)].map((match) => Number(match[1]));
  assert.ok(shards.length >= 2, "tests are sharded");
  assert.deepEqual(shards, shards.map((_, index) => index + 1), "shards are contiguous from 1");
  const partitions = [...linuxRust.matchAll(/--partition count:\$\{\{ matrix\.shard \}\}\/(\d+)/g)].map((match) => Number(match[1]));
  assert.deepEqual(partitions, [shards.length], "one nextest invocation partitions by the matrix size");
  for (const shard of shards) assert.match(linuxRust, new RegExp(`label: tests ${shard}/${shards.length}\\b`));
});

test("doctests, OpenAPI freshness and lint still run, and the job stays aggregated", () => {
  assert.match(linuxRust, /cargo test --workspace --doc/);
  assert.match(linuxRust, /run: pnpm check:openapi/);
  assert.match(linuxRust, /cargo clippy --workspace --all-targets -- -D warnings/);
  // The aggregate must still depend on the sharded job (other required jobs,
  // such as web-e2e, may sit beside it in the list).
  assert.match(ci, /ci-required:[\s\S]*needs: \[[^\]]*\blinux-rust\b[^\]]*\]/);
  assert.doesNotMatch(linuxRust, /--no-tests|continue-on-error|--retries/, "nothing is skipped, retried or tolerated");
});

test("nextest fails a hung test by name within a bounded time", () => {
  const timeout = nextestConfig.match(/slow-timeout = \{ period = "(\d+)s", terminate-after = (\d+) \}/);
  assert.ok(timeout, "default profile sets period and terminate-after");
  assert.ok(Number(timeout[1]) * Number(timeout[2]) <= 600, "a hung test is killed within ten minutes");
  assert.match(nextestConfig, /\[profile\.ci\][\s\S]*inherits = "default"/);
  assert.match(nextestConfig, /fail-fast = false/);
});
