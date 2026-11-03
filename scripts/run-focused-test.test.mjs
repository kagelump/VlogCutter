import assert from "node:assert/strict";
import { after, test } from "node:test";
import { runFocusedTest } from "./run-focused-test.mjs";

const expected = ["named test and count pass", "zero tests fail", "wrong test name fails", "zero count fails"];
const observed = new Set();

function runScenario(name, { output, status = 0 } = {}) {
  observed.add(name);
  let captured = "";
  let invocation;
  const code = runFocusedTest({
    args: ["rcdvlog-core", "chronology::keeps_order"],
    spawn: (command, args, options) => {
      invocation = { command, args, options };
      return { status, stdout: output ?? "", stderr: "" };
    },
    write: (chunk) => { captured += chunk; },
  });
  assert.equal(invocation.command, "cargo");
  assert.deepEqual(invocation.args, ["+1.97.1", "nextest", "run", "-p", "rcdvlog-core", "-E", "test(=chronology::keeps_order)"]);
  assert.equal(invocation.options.env.RUSTUP_TOOLCHAIN, "1.97.1");
  return { code, output: captured };
}

test("named test result and positive executed count pass", () => {
  const result = runScenario("named test and count pass", {
    output: "        PASS [   0.004s] rcdvlog-core chronology::keeps_order\n     Summary [   0.005s] 1 test run: 1 passed, 19 skipped\n",
  });
  assert.equal(result.code, 0);
  assert.match(result.output, /observed 1 result\(s\) named chronology::keeps_order; 1 executed test\(s\)/);
});

test("zero matching tests fail even when cargo exits successfully", () => {
  const result = runScenario("zero tests fail", {
    output: "     Summary [   0.001s] 0 tests run: 0 passed, 20 skipped\n",
  });
  assert.equal(result.code, 1);
  assert.match(result.output, /observed 0 matching result\(s\), 0 executed test\(s\)/);
});

test("a different passing test name does not satisfy the request", () => {
  const result = runScenario("wrong test name fails", {
    output: "        PASS [   0.004s] rcdvlog-core chronology::other_test\n     Summary [   0.005s] 1 test run: 1 passed, 19 skipped\n",
  });
  assert.equal(result.code, 1);
  assert.match(result.output, /expected test chronology::keeps_order/);
});

test("reported zero count fails even if expected name appears", () => {
  const result = runScenario("zero count fails", {
    output: "        PASS [   0.004s] rcdvlog-core chronology::keeps_order\n     Summary [   0.001s] 0 tests run: 0 passed, 20 skipped\n",
  });
  assert.equal(result.code, 1);
});

after(() => {
  assert.ok(observed.size > 0, "focused Rust test helper must execute tests");
  assert.equal(expected.length, 4, "all expected-name and executed-count cases must be present");
  assert.deepEqual([...observed].sort(), [...expected].sort());
});
