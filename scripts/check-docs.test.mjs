// Regression test: check-docs.mjs must find and check markdown files when
// invoked from inside a git worktree (where .git is a file, not a directory).
//
// Run with: node --test scripts/check-docs.test.mjs

import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { describe, it } from "node:test";
import assert from "node:assert/strict";

const SCRIPT = resolve(import.meta.dirname, "check-docs.mjs");

/**
 * Create a minimal fixture directory that simulates a git worktree:
 * - .git is a file (not a directory) — the worktree marker
 * - Required documentation artifacts are stubbed so the check can pass
 * - Test markdown files exist to verify walk() finds them
 */
function createFixture() {
  const root = mkdtempSync(join(tmpdir(), "check-docs-test-"));
  // Simulate a git worktree: .git as a file pointing to the real repo.
  writeFileSync(join(root, ".git"), "gitdir: /nonexistent/.git/worktrees/test\n");

  // Stub the required documentation artifacts.
  const requiredDirs = [
    "docs/server",
    "docs/tasks/server/schemas",
  ];
  for (const dir of requiredDirs) {
    mkdirSync(join(root, dir), { recursive: true });
  }

  const requiredMdFiles = [
    "docs/server/README.md",
    "docs/server/deployment.md",
    "docs/server/security.md",
    "docs/server/storage-and-library.md",
    "docs/server/sync.md",
    "docs/server/jobs-and-media.md",
    "docs/server/backup-and-recovery.md",
    "docs/server/api.md",
    "docs/server/development-and-testing.md",
    "docs/server/troubleshooting.md",
  ];
  for (const f of requiredMdFiles) {
    writeFileSync(join(root, f), `# ${f}\n`);
  }

  const requiredJsonFiles = [
    "docs/tasks/server/schemas/baseline-v1.schema.json",
    "docs/tasks/server/schemas/reconcile-v1.schema.json",
    "docs/tasks/server/schemas/reconcile-response-v1.schema.json",
  ];
  for (const f of requiredJsonFiles) {
    writeFileSync(join(root, f), "{}");
  }

  // Create some test markdown files with content to exercise the checks.
  writeFileSync(join(root, "readme.md"), "# Test readme\n[valid link](./other.md)\n");
  writeFileSync(join(root, "other.md"), "# Other file\n");

  // Create a subdirectory with a nested .git file to verify the worktree-skip
  // still applies to subdirectories discovered during the walk.
  const nestedWorktree = join(root, "nested-wt");
  mkdirSync(nestedWorktree);
  writeFileSync(join(nestedWorktree, ".git"), "gitdir: /nonexistent/.git/worktrees/nested\n");
  writeFileSync(join(nestedWorktree, "should-be-skipped.md"), "# Should not be checked\n");

  return root;
}

describe("check-docs.mjs worktree regression", () => {
  it("finds markdown files when invoked from inside a simulated git worktree", () => {
    const fixture = createFixture();
    try {
      const stdout = execFileSync("node", [SCRIPT], {
        cwd: fixture,
        encoding: "utf8",
      });

      const match = stdout.match(/Documentation check passed \((\d+) Markdown files\)/);
      assert.ok(match, `Expected success message, got: ${stdout}`);
      const count = parseInt(match[1], 10);
      assert.ok(count >= 2, `Expected at least 2 markdown files, got ${count}`);
    } finally {
      rmSync(fixture, { recursive: true, force: true });
    }
  });

  it("exits 0 when all checks pass in a simulated worktree", () => {
    const fixture = createFixture();
    try {
      assert.doesNotThrow(() => {
        execFileSync("node", [SCRIPT], { cwd: fixture, encoding: "utf8" });
      }, "Expected exit code 0 from a simulated worktree");
    } finally {
      rmSync(fixture, { recursive: true, force: true });
    }
  });

  it("still skips nested git worktrees discovered during recursion", () => {
    const fixture = createFixture();
    try {
      const stdout = execFileSync("node", [SCRIPT], {
        cwd: fixture,
        encoding: "utf8",
      });

      // The nested-wt/should-be-skipped.md should NOT appear in the count.
      // Our fixture has: readme.md, other.md, + 10 required stubs = 12.
      // The nested-wt file should be skipped.
      const match = stdout.match(/Documentation check passed \((\d+) Markdown files\)/);
      assert.ok(match, `Expected success message, got: ${stdout}`);
      const count = parseInt(match[1], 10);
      assert.equal(count, 12, `Expected 12 files (stubs + fixture .md files), got ${count}`);
    } finally {
      rmSync(fixture, { recursive: true, force: true });
    }
  });
});
