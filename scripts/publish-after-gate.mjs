#!/usr/bin/env node

import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { runGate } from "./pre-pr-gate.mjs";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const actions = {
  push: [["git", ["push", "--set-upstream", "origin", "HEAD"]]],
  pr: [
    ["git", ["push", "--set-upstream", "origin", "HEAD"]],
    ["gh", ["pr", "create", "--fill"]],
  ],
};

function snapshot(spawn) {
  const status = spawn("git", ["status", "--porcelain=v1", "-z"], { cwd: root, encoding: "utf8" });
  if (status.status !== 0 || status.error) return null;
  if (status.stdout.length > 0) return { dirty: true };
  const head = spawn("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" });
  const tracked = spawn("git", ["diff", "--binary", "HEAD"], { cwd: root, encoding: "utf8" });
  const untracked = spawn("git", ["ls-files", "--others", "--exclude-standard", "-z"], { cwd: root, encoding: "utf8" });
  if ([head, tracked, untracked].some((result) => result.status !== 0 || result.error)) return null;
  const untrackedContent = [];
  for (const path of untracked.stdout.split("\0").filter(Boolean)) {
    const hash = spawn("git", ["hash-object", "--", path], { cwd: root, encoding: "utf8" });
    if (hash.status !== 0 || hash.error) return null;
    untrackedContent.push(`${path}\0${hash.stdout.trim()}`);
  }
  return { dirty: false, head: head.stdout.trim(), tracked: tracked.stdout, untracked: untrackedContent.join("\0") };
}

export function publishAfterGate({ action, gate = runGate, spawn = spawnSync, write = (value) => process.stdout.write(value) }) {
  const commands = actions[action];
  if (!commands) {
    write("Usage: node scripts/publish-after-gate.mjs push|pr\n");
    return 2;
  }

  const before = snapshot(spawn);
  if (!before) {
    write("Cannot read HEAD and working-tree state before the gate; refusing publication.\n");
    return 1;
  }
  if (before.dirty) {
    write("Working tree is dirty before the gate; commit or discard changes before publication.\n");
    return 1;
  }
  write(`Publication candidate HEAD: ${before.head}\n`);
  // The wrapper's action argument is not an argument to the verification gate.
  const gateStatus = gate({ args: [] });
  if (gateStatus !== 0) {
    write(`Pre-PR gate failed with exit ${gateStatus}; refusing publication.\n`);
    return gateStatus || 1;
  }

  for (const [command, args] of commands) {
    const current = snapshot(spawn);
    if (!current || before.head !== current.head || before.tracked !== current.tracked || before.untracked !== current.untracked) {
      write("HEAD or working-tree state changed since the gate; refusing publication.\n");
      return 1;
    }
    write(`Gate passed; running ${command} ${args.join(" ")}.\n`);
    const result = spawn(command, args, { cwd: root, encoding: "utf8", stdio: "inherit" });
    if (result.error) {
      write(`${result.error.message}\n`);
      return 127;
    }
    if (result.status !== 0) return result.status ?? 1;
  }
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = publishAfterGate({ action: process.argv[2] });
}
