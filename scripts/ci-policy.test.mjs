import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, mkdirSync, writeFileSync, renameSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { classifyChanges, decidePlatform, linuxOnly } from "./ci-platform.mjs";
import { requiredFailures } from "./ci-required.mjs";

const modified = (path) => ({ status: "M", path });
test("only modified, explicitly isolated Linux configs may skip macOS", () => {
  assert.equal(classifyChanges([...linuxOnly].map(modified)).macos, false);
  for (const path of ["apps/server/src/main.rs", "core/rcdvlog-core/src/lib.rs", "apps/desktop/src/App.tsx", "packages/review-ui/src/index.ts", "Cargo.lock", "Cargo.toml", "pnpm-lock.yaml", "scripts/new.mjs", ".github/workflows/ci.yml", "docs/server/README.md", "deploy/server/Dockerfile", "deploy/server/new.conf", "unknown"]) {
    assert.equal(classifyChanges([modified(path)]).macos, true, path);
    assert.equal(classifyChanges([modified("deploy/server/Caddyfile"), modified(path)]).macos, true, `mixed ${path}`);
  }
  for (const status of ["A", "D", "R100", "C100", "T", "U", ""]) {
    assert.equal(classifyChanges([{ status, path: "deploy/server/Caddyfile" }]).macos, true, status);
  }
  assert.equal(classifyChanges([]).macos, true);
  assert.equal(classifyChanges(null).macos, true);
});
const base = "a".repeat(40), head = "b".repeat(40);
test("PRs (including forks) use merge-base diff; pushes use before/after", () => {
  for (const fork of [false, true]) {
    const result = decidePlatform("pull_request", { pull_request: { base: { sha: base }, head: { sha: head, repo: { fork } } } }, (args) => {
      assert.deepEqual(args, ["diff", "--no-renames", "--name-status", "-z", `${base}...${head}`]);
      return "M\0deploy/server/Caddyfile\0";
    });
    assert.equal(result.macos, false);
  }
  assert.equal(decidePlatform("push", { before: base, after: head }, (args) => {
    assert.equal(args.at(-1), `${base}..${head}`);
    return "M\0deploy/server/compose.yaml\0";
  }).macos, false);
});
test("missing objects, malformed diffs/events, initial pushes and full runs fail closed", () => {
  for (const event of ["workflow_dispatch", "schedule", "merge_group", "unknown"]) {
    assert.equal(decidePlatform(event, {}).macos, true);
  }
  for (const event of [{}, { before: "0".repeat(40), after: head }, { before: "--help", after: head }]) {
    assert.equal(decidePlatform("push", event).macos, true);
  }
  const event = { before: base, after: head };
  assert.equal(decidePlatform("push", event, () => { throw new Error("object absent"); }).macos, true);
  for (const diff of ["", "M\0deploy/server/Caddyfile", "M\0", "M\0deploy/server/Caddyfile\0garbage\0"]) {
    assert.equal(decidePlatform("push", event, () => diff).macos, true, JSON.stringify(diff));
  }
});
test("real Git treats renames/new paths conservatively even inside the allowlist", () => {
  const dir = mkdtempSync(join(tmpdir(), "ci-policy-"));
  const git = (args) => {
    const result = spawnSync("git", args, { cwd: dir, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  try {
    git(["init", "-q"]);
    git(["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "--allow-empty", "-qm", "base"]);
    const sha = git(["rev-parse", "HEAD"]);
    const run = (eventName, event) => decidePlatform(eventName, event, (args) => {
      const result = spawnSync("git", args, { cwd: dir, encoding: "utf8" });
      if (result.status !== 0) throw new Error("missing");
      return result.stdout;
    });
    assert.equal(run("pull_request", { pull_request: { base: { sha }, head: { sha: head } } }).macos, true);
    mkdirSync(join(dir, "deploy/server"), { recursive: true });
    writeFileSync(join(dir, "deploy/server/Caddyfile"), "base config\n");
    git(["add", "."]);
    git(["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "add"]);
    const added = git(["rev-parse", "HEAD"]);
    assert.equal(run("push", { before: sha, after: added }).macos, true);
    writeFileSync(join(dir, "deploy/server/Caddyfile"), "modified config\n");
    git(["add", "."]);
    git(["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "modify"]);
    const modified = git(["rev-parse", "HEAD"]);
    assert.equal(run("push", { before: added, after: modified }).macos, false);
    assert.equal(run("pull_request", { pull_request: { base: { sha: added }, head: { sha: modified } } }).macos, false);
    renameSync(join(dir, "deploy/server/Caddyfile"), join(dir, "deploy/server/compose.yaml"));
    git(["add", "."]);
    git(["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "rename"]);
    assert.equal(run("push", { before: modified, after: git(["rev-parse", "HEAD"]) }).macos, true);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
const green = () => Object.fromEntries(["changes", "linux-rust", "frontend", "browser", "macos"].map((name) => [name, { result: "success" }]));
test("aggregate accepts all green or an explicitly justified macOS skip", () => {
  assert.deepEqual(requiredFailures(green()), []);
  const needs = green();
  needs.changes.outputs = { macos: "false" };
  needs.macos.result = "skipped";
  assert.deepEqual(requiredFailures(needs), []);
});
test("aggregate rejects failure, cancellation, missing and unjustified skips in every required job", () => {
  for (const job of Object.keys(green())) {
    for (const result of ["failure", "cancelled", "skipped", undefined]) {
      const needs = green();
      needs[job] = { result };
      assert.ok(requiredFailures(needs).includes(job), `${job}: ${result}`);
    }
    const needs = green(); delete needs[job];
    assert.ok(requiredFailures(needs).includes(job));
  }
  assert.equal(requiredFailures({}).length, 5);
  const needs = green(); needs.changes = { result: "failure", outputs: { macos: "false" } }; needs.macos.result = "skipped";
  assert.ok(requiredFailures(needs).includes("changes"));
});
