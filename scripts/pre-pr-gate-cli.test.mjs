import assert from "node:assert/strict";
import { chmodSync, copyFileSync, mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

const source = fileURLToPath(new URL("./pre-pr-gate.mjs", import.meta.url));
const expected = ["valid CLI pass", "format command failure", "clippy command failure", "test command failure preserves output", "frontend command failure", "missing command fails closed", "clean committed candidate with whitespace fails", "default local requires plan", "structured local plan runs", "failed focused check blocks gate"];
const observed = new Set();

function runCli(name, { fail = "", omit = "", args = ["--scope", "pre-pr"], plan = null } = {}) {
  observed.add(name);
  const temp = mkdtempSync(join(tmpdir(), "vlogcutter-gate-cli-"));
  const scripts = join(temp, "scripts");
  const workflow = join(temp, ".github", "workflows");
  const bin = join(temp, "fake-bin");
  mkdirSync(scripts, { recursive: true });
  mkdirSync(workflow, { recursive: true });
  mkdirSync(bin);
  copyFileSync(source, join(scripts, "pre-pr-gate.mjs"));
  writeFileSync(join(workflow, "ci.yml"), 'env:\n  RUST_TOOLCHAIN: "1.97.1"\n');
  if (plan) writeFileSync(join(temp, "checks.json"), JSON.stringify(plan));

  const commands = {
    git: '#!/bin/sh\nexit 0\n',
    rustc: '#!/bin/sh\necho "rustc 1.97.1 (fake)"\n',
    cargo: `#!/bin/sh
case "$2" in
  fmt) if [ "$FAKE_FAIL" = format ]; then echo "forced fmt failure" >&2; exit 11; fi ;;
  clippy) if [ "$FAKE_FAIL" = clippy ]; then echo "forced clippy failure" >&2; exit 12; fi ;;
  test)
    echo "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
    if [ "$FAKE_FAIL" = test ]; then echo "forced cargo test failure" >&2; exit 13; fi
    ;;
esac
if [ "$1" = test ]; then
  echo "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
  if [ "$FAKE_FAIL" = focused ]; then echo "forced focused failure" >&2; exit 15; fi
fi
exit 0
`,
    pnpm: `#!/bin/sh
if [ "$FAKE_FAIL" = frontend ] && [ "$1" = check:frontend ]; then echo "forced frontend failure" >&2; exit 14; fi
exit 0
`,
  };
  for (const [command, body] of Object.entries(commands)) {
    if (command === omit) continue;
    const path = join(bin, command);
    writeFileSync(path, body, { mode: 0o755 });
    chmodSync(path, 0o755);
  }

  try {
    return spawnSync(process.execPath, [join(scripts, "pre-pr-gate.mjs"), ...args.map((arg) => arg === "$PLAN" ? join(temp, "checks.json") : arg)], {
      cwd: temp,
      encoding: "utf8",
      env: { ...process.env, PATH: bin, FAKE_FAIL: fail },
      maxBuffer: 2 * 1024 * 1024,
    });
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
}

test("CLI succeeds when every fake prerequisite and check succeeds", () => {
  const result = runCli("valid CLI pass");
  assert.equal(result.status, 0, JSON.stringify(result));
  assert.match(result.stdout, /Gate summary \(pre-pr\): 18 passed, 0 failed\./);
});

test("CLI defaults to local scope and requires a focused checks plan", () => {
  const result = runCli("default local requires plan", { args: [] });
  assert.equal(result.status, 2);
  assert.match(result.stdout, /Local scope requires --checks/);
  assert.doesNotMatch(result.stdout, /===/);
});

test("CLI runs commands from a valid structured local plan", () => {
  const result = runCli("structured local plan runs", {
    args: ["--checks", "$PLAN"],
    plan: { checks: [{ name: "focused cargo tests", command: "cargo", args: ["test", "-p", "fixture"] }] },
  });
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stdout, /=== focused cargo tests ===/);
  assert.match(result.stdout, /Gate summary \(local\): 8 passed, 0 failed\./);
});

test("CLI reports focused failure and blocks gate success", () => {
  const result = runCli("failed focused check blocks gate", {
    fail: "focused",
    args: ["--checks", "$PLAN"],
    plan: { checks: [{ name: "focused cargo tests", command: "cargo", args: ["test", "-p", "fixture"] }] },
  });
  assert.equal(result.status, 1);
  assert.match(result.stdout, /RESULT focused cargo tests: FAIL \(exit 15\)/);
});

test("CLI exits nonzero when the fake formatter fails", () => {
  const result = runCli("format command failure", { fail: "format" });
  assert.equal(result.status, 1);
  assert.match(result.stdout, /RESULT cargo fmt .*: FAIL \(exit 11\)/);
});

test("CLI exits nonzero when fake clippy fails", () => {
  const result = runCli("clippy command failure", { fail: "clippy" });
  assert.equal(result.status, 1);
  assert.match(result.stdout, /RESULT cargo clippy .*: FAIL \(exit 12\)/);
});

test("CLI preserves cargo test output and exits nonzero on failure", () => {
  const result = runCli("test command failure preserves output", { fail: "test" });
  assert.equal(result.status, 1);
  assert.match(result.stdout, /test result: ok\. 1 passed; 0 failed/);
  assert.match(result.stdout, /forced cargo test failure/);
  assert.match(result.stdout, /RESULT cargo test --workspace: FAIL \(exit 13\)/);
});

test("CLI exits nonzero when the fake frontend check fails", () => {
  const result = runCli("frontend command failure", { fail: "frontend" });
  assert.equal(result.status, 1);
  assert.match(result.stdout, /RESULT pnpm check:frontend: FAIL \(exit 14\)/);
});

test("CLI fails closed when cargo is missing from PATH", () => {
  const result = runCli("missing command fails closed", { omit: "cargo" });
  assert.equal(result.status, 1);
  assert.match(result.stdout, /RESULT cargo fmt --all -- --check: FAIL/);
  assert.match(result.stdout, /spawnSync cargo ENOENT/);
});

test("a clean feature commit with trailing whitespace fails the committed-candidate check", () => {
  observed.add("clean committed candidate with whitespace fails");
  const temp = mkdtempSync(join(tmpdir(), "vlogcutter-gate-git-"));
  const scripts = join(temp, "scripts");
  const workflow = join(temp, ".github", "workflows");
  const bin = join(temp, "fake-bin");
  mkdirSync(scripts, { recursive: true });
  mkdirSync(workflow, { recursive: true });
  mkdirSync(bin);
  copyFileSync(source, join(scripts, "pre-pr-gate.mjs"));
  writeFileSync(join(workflow, "ci.yml"), 'env:\n  RUST_TOOLCHAIN: "1.97.1"\n');
  writeFileSync(join(bin, "rustc"), '#!/bin/sh\necho "rustc 1.97.1 (fake)"\n', { mode: 0o755 });
  writeFileSync(join(bin, "cargo"), `#!/bin/sh
case "$2" in
  test) echo "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out" ;;
esac
exit 0
`, { mode: 0o755 });
  writeFileSync(join(bin, "pnpm"), "#!/bin/sh\nexit 0\n", { mode: 0o755 });

  const git = (args) => {
    const result = spawnSync("git", args, { cwd: temp, encoding: "utf8" });
    assert.equal(result.status, 0, `${args.join(" ")}: ${result.stderr}`);
    return result.stdout.trim();
  };

  try {
    git(["init", "-b", "main"]);
    git(["config", "user.name", "Gate Test"]);
    git(["config", "user.email", "gate-test@example.invalid"]);
    // Commit the fixture and fake prerequisites first so the feature worktree
    // is genuinely clean and only the committed candidate diff is under test.
    git(["add", "."]);
    git(["commit", "-m", "fixture base"]);
    const base = git(["rev-parse", "HEAD"]);
    writeFileSync(join(temp, "candidate.txt"), "committed candidate has trailing whitespace \n");
    git(["add", "candidate.txt"]);
    git(["commit", "-m", "feature candidate"]);
    git(["update-ref", "refs/remotes/origin/main", base]);
    assert.equal(git(["status", "--porcelain=v1"]), "", "feature worktree must be clean");

    const result = spawnSync(process.execPath, [join(scripts, "pre-pr-gate.mjs"), "--scope", "pre-pr"], {
      cwd: temp,
      encoding: "utf8",
      env: { ...process.env, PATH: `${bin}:${process.env.PATH}` },
      maxBuffer: 2 * 1024 * 1024,
    });
    assert.equal(result.status, 1, `${result.stdout}\n${result.stderr}`);
    assert.match(result.stdout, /RESULT git diff --check origin\/main\.\.\.HEAD: FAIL/);
    assert.match(result.stdout, /trailing whitespace/);
    assert.match(result.stdout, /fix the trailing-whitespace lines shown above/);
    assert.doesNotMatch(result.stdout, /git fetch origin main/);
    assert.match(result.stdout, /Gate summary \(pre-pr\): 17 passed, 1 failed\./);
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});

after(() => {
  assert.ok(observed.size > 0, "the subprocess regression must invoke the CLI");
  assert.equal(expected.length, 10, "all expected CLI subprocess scenarios must remain present");
  assert.deepEqual([...observed].sort(), [...expected].sort());
});
