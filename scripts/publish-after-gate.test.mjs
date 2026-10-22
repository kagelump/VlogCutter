import assert from "node:assert/strict";
import { after, test } from "node:test";
import { publishAfterGate } from "./publish-after-gate.mjs";

const expected = ["dirty tree blocks before gate", "gate failure blocks publishing", "changed tree blocks publishing", "changed head blocks publishing", "changed untracked content blocks publishing", "unchanged tree permits fixed PR command", "PR publication stops when push fails", "HEAD change during push prevents PR"];
const observed = new Set();

function attempt({ gateStatus = 0, dirty = false, changeTree = false, changeHead = false, changeUntracked = false, failPush = false, changeHeadAfterPush = false, action = "pr", gateArgs = ["--scope", "pre-pr"] } = {}) {
  const calls = [];
  let headChangedAfterPush = false;
  let output = "";
  const spawn = (command, args) => {
    calls.push([command, args]);
    if (command === "git" && args[0] === "push" && changeHeadAfterPush) {
      headChangedAfterPush = true;
      return { status: 0 };
    }
    if (command === "git" && args[0] === "push" && failPush) return { status: 8 };
    if (command === "git" && args[0] === "status") return { status: 0, stdout: dirty ? " M file\0" : "" };
    if (command === "git" && args[0] === "rev-parse") return { status: 0, stdout: headChangedAfterPush || changeHead && calls.filter(([name, argv]) => name === "git" && argv[0] === "rev-parse").length > 1 ? "def456\n" : "abc123\n" };
    if (command === "git" && args[0] === "diff") return { status: 0, stdout: changeTree && calls.filter(([name, argv]) => name === "git" && argv[0] === "diff").length > 1 ? "modified bytes\n" : "" };
    if (command === "git" && args[0] === "ls-files") return { status: 0, stdout: "new-file.md\0" };
    if (command === "git" && args[0] === "hash-object") return { status: 0, stdout: changeUntracked && calls.filter(([name, argv]) => name === "git" && argv[0] === "hash-object").length > 1 ? "hash-b\n" : "hash-a\n" };
    return { status: 0 };
  };
  const code = publishAfterGate({
    action,
    gateArgs,
    gate: (options) => { assert.deepEqual(options.args, gateArgs); calls.push(["gate", gateArgs]); return gateStatus; },
    spawn,
    write: (chunk) => { output += chunk; },
  });
  return { calls, code, output };
}

test("dirty tree is rejected before running the gate", () => {
  observed.add("dirty tree blocks before gate");
  const result = attempt({ dirty: true });
  assert.equal(result.code, 1);
  assert.equal(result.calls.some(([name]) => name === "gate" || name === "gh"), false);
  assert.match(result.output, /Working tree is dirty before the gate/);
});

test("gate failure blocks publishing", () => {
  observed.add("gate failure blocks publishing");
  for (const action of ["push", "pr"]) {
    const result = attempt({ gateStatus: 1, action });
    assert.equal(result.code, 1);
    assert.equal(result.calls.some(([name, args]) => name === "gh" || name === "git" && args[0] === "push"), false);
    assert.match(result.output, /refusing publication/);
  }
});

test("changed tree blocks publishing", () => {
  observed.add("changed tree blocks publishing");
  for (const action of ["push", "pr"]) {
    const result = attempt({ changeTree: true, action });
    assert.equal(result.code, 1);
    assert.equal(result.calls.some(([name, args]) => name === "gh" || name === "git" && args[0] === "push"), false);
    assert.match(result.output, /HEAD or working-tree state changed since the gate/);
  }
});

test("changed head blocks publishing", () => {
  observed.add("changed head blocks publishing");
  const result = attempt({ changeHead: true });
  assert.equal(result.code, 1);
  assert.equal(result.calls.some(([name]) => name === "gh"), false);
  assert.match(result.output, /HEAD or working-tree state changed since the gate/);
});

test("changed untracked content blocks publishing", () => {
  observed.add("changed untracked content blocks publishing");
  const result = attempt({ changeUntracked: true });
  assert.equal(result.code, 1);
  assert.equal(result.calls.some(([name]) => name === "gh"), false);
  assert.match(result.output, /HEAD or working-tree state changed since the gate/);
});

test("unchanged tree permits fixed PR command", () => {
  observed.add("unchanged tree permits fixed PR command");
  const result = attempt();
  assert.equal(result.code, 0);
  assert.match(result.output, /Publication candidate HEAD: abc123/);
  assert.deepEqual(result.calls.filter(([name, args]) => name === "gh" || name === "git" && args[0] === "push"), [
    ["git", ["push", "--set-upstream", "origin", "HEAD"]],
    ["gh", ["pr", "create", "--fill"]],
  ]);
});

test("PR creation is skipped when its preceding push fails", () => {
  observed.add("PR publication stops when push fails");
  const result = attempt({ failPush: true });
  assert.equal(result.code, 8);
  assert.equal(result.calls.some(([name]) => name === "gh"), false);
  assert.deepEqual(result.calls.at(-1), ["git", ["push", "--set-upstream", "origin", "HEAD"]]);
});

test("PR creation is skipped when push changes the candidate HEAD", () => {
  observed.add("HEAD change during push prevents PR");
  const result = attempt({ changeHeadAfterPush: true });
  assert.equal(result.code, 1);
  assert.equal(result.calls.some(([name]) => name === "gh"), false);
  assert.match(result.output, /HEAD or working-tree state changed since the gate/);
});

test("push establishes the current branch upstream explicitly", () => {
  const result = attempt({ action: "push" });
  assert.equal(result.code, 0);
  assert.deepEqual(result.calls.at(-1), ["git", ["push", "--set-upstream", "origin", "HEAD"]]);
});

test("publication rejects fast, missing plans, unknown and repeated options before snapshot", () => {
  for (const gateArgs of [["--scope", "fast"], [], ["--scope", "fast", "--checks", "x"], ["--scope", "pre-pr", "--scope", "local"], ["--scope", "local", "--checks", "x", "--checks", "y"], ["--scope", "pre-pr", "--checks", "x"], ["--oops"]]) {
    const result = attempt({ gateArgs });
    assert.equal(result.code, 2);
    assert.equal(result.calls.some(([name]) => name === "gate" || name === "gh"), false);
  }
});

test("local publication forwards the plan and scope to the gate", () => {
  const result = attempt({ gateArgs: ["--scope", "local", "--checks", "/tmp/focused.json"] });
  assert.equal(result.code, 0);
  assert.ok(result.calls.some(([name, args]) => name === "gate" && args.join(" ") === "--scope local --checks /tmp/focused.json"));
});

test("failed focused local gate blocks both publication actions", () => {
  for (const action of ["push", "pr"]) {
    const result = attempt({ action, gateStatus: 1, gateArgs: ["--scope", "local", "--checks", "/tmp/focused.json"] });
    assert.equal(result.code, 1);
    assert.equal(result.calls.some(([name, args]) => name === "gh" || name === "git" && args[0] === "push"), false);
  }
});

after(() => {
  assert.ok(observed.size > 0, "publication regression helper must execute tests");
  assert.equal(expected.length, 8, "all fixed publication boundary scenarios must remain present");
  assert.deepEqual([...observed].sort(), [...expected].sort());
});
