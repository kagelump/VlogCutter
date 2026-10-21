import assert from "node:assert/strict";
import { after, test } from "node:test";
import { runGate } from "./pre-pr-gate.mjs";

const expectedScenarios = [
  "clean pass",
  "format failure",
  "clippy failure",
  "test failure retains output",
  "frontend failure",
  "missing cargo prerequisite",
  "wrong rust version is rejected",
  "successful test command with zero tests",
];
const observedScenarios = new Set();

function runScenario(name, { fail, missingCargo = false, emptyTests = false, wrongRust = false } = {}) {
  observedScenarios.add(name);
  let output = "";
  const spawn = (command, args, options) => {
    assert.equal(options.env.RUSTUP_TOOLCHAIN, "1.97.1", "nested cargo commands inherit the CI pin");
    if (command === "cargo" && missingCargo) {
      return { status: null, stdout: "", stderr: "", error: { message: "spawnSync cargo ENOENT" } };
    }
    if (command === "rustc") return { status: 0, stdout: `rustc ${wrongRust ? "1.85.0" : "1.97.1"} (fake)\n`, stderr: "" };
    if (command === "cargo" && args.includes("test")) {
      const summary = emptyTests
        ? "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
        : "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
      return { status: fail === "test" ? 9 : 0, stdout: summary, stderr: fail === "test" ? "forced test failure\n" : "" };
    }
    if (command === "cargo" && args.includes("fmt")) {
      return { status: fail === "format" ? 9 : 0, stdout: "", stderr: fail === "format" ? "forced format failure\n" : "" };
    }
    if (command === "cargo" && args.includes("clippy")) {
      return { status: fail === "clippy" ? 9 : 0, stdout: "", stderr: fail === "clippy" ? "forced clippy failure\n" : "" };
    }
    if (command === "pnpm" && fail === "frontend" && args[0] === "check:frontend") {
      return { status: 8, stdout: "", stderr: "forced frontend failure\n" };
    }
    return { status: 0, stdout: "", stderr: "" };
  };
  const code = runGate({ spawn, write: (chunk) => { output += chunk; } });
  return { code, output };
}

test("clean pass runs every required pre-PR check", () => {
  const result = runScenario("clean pass");
  assert.equal(result.code, 0, result.output);
  assert.match(result.output, /RESULT cargo fmt --all -- --check: PASS/);
  assert.match(result.output, /RESULT git diff --cached --check: PASS/);
  assert.match(result.output, /RESULT cargo clippy --workspace --all-targets -- -D warnings: PASS/);
  assert.match(result.output, /RESULT cargo test --workspace: PASS/);
  assert.match(result.output, /RESULT pnpm check:frontend: PASS/);
  assert.match(result.output, /Gate summary \(pre-pr\): 17 passed, 0 failed\./);
});

test("format failure appears in summary and fails the gate", () => {
  const result = runScenario("format failure", { fail: "format" });
  assert.equal(result.code, 1);
  assert.match(result.output, /RESULT cargo fmt --all -- --check: FAIL \(exit 9\)/);
  assert.match(result.output, /Gate summary \(pre-pr\): 16 passed, 1 failed\./);
});

test("clippy failure appears in summary and fails the gate", () => {
  const result = runScenario("clippy failure", { fail: "clippy" });
  assert.equal(result.code, 1);
  assert.match(result.output, /RESULT cargo clippy .*: FAIL \(exit 9\)/);
});

test("test failure keeps its test result output and fails the gate", () => {
  const result = runScenario("test failure retains output", { fail: "test" });
  assert.equal(result.code, 1);
  assert.match(result.output, /test result: ok\. 3 passed; 0 failed/);
  assert.match(result.output, /RESULT cargo test --workspace: FAIL \(exit 9\)/);
});

test("frontend failure appears in summary and fails the gate", () => {
  const result = runScenario("frontend failure", { fail: "frontend" });
  assert.equal(result.code, 1);
  assert.match(result.output, /RESULT pnpm check:frontend: FAIL \(exit 8\)/);
});

test("missing cargo prerequisite is reported and fails closed", () => {
  const result = runScenario("missing cargo prerequisite", { missingCargo: true });
  assert.equal(result.code, 1);
  assert.match(result.output, /RESULT cargo fmt --all -- --check: FAIL/);
  assert.match(result.output, /spawnSync cargo ENOENT/);
});

test("a successful rustc command at the wrong version is rejected", () => {
  const result = runScenario("wrong rust version is rejected", { wrongRust: true });
  assert.equal(result.code, 1);
  assert.match(result.output, /rustc did not report the required 1\.97\.1 toolchain/);
});

test("a successful cargo command with zero reported tests fails closed", () => {
  const result = runScenario("successful test command with zero tests", { emptyTests: true });
  assert.equal(result.code, 1);
  assert.match(result.output, /cargo test exited 0 without reporting any executed tests/);
});

after(() => {
  assert.ok(observedScenarios.size > 0, "the focused regression helper must execute tests");
  assert.equal(expectedScenarios.length, 8, "the regression matrix must retain its expected minimum test count");
  assert.deepEqual([...observedScenarios].sort(), [...expectedScenarios].sort(), "every expected named scenario must execute");
});
