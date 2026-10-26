#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const ci = readFileSync(resolve(root, ".github/workflows/ci.yml"), "utf8");
const toolchainMatch = ci.match(/^  RUST_TOOLCHAIN: "([^"]+)"$/m);
if (!toolchainMatch) throw new Error("Cannot determine pinned RUST_TOOLCHAIN from .github/workflows/ci.yml");
const rustToolchain = toolchainMatch[1];

export function runGate({ args = process.argv.slice(2), spawn = spawnSync, write = (value) => process.stdout.write(value) } = {}) {
  let scope = "local";
  let checksPath;
  let scopeSeen = false;
  let checksSeen = false;
  for (let i = 0; i < args.length; i += 1) {
    if (args[i] === "--scope" && args[i + 1]) {
      if (scopeSeen) { write("Repeated --scope option.\n"); return 2; }
      scopeSeen = true;
      scope = args[++i];
    } else if (args[i] === "--checks" && args[i + 1]) {
      if (checksSeen) { write("Repeated --checks option.\n"); return 2; }
      checksSeen = true;
      checksPath = args[++i];
    } else {
      write(`Unknown argument: ${args[i]}\nUsage: pnpm gate [--scope local|fast|pre-pr] [--checks <JSON file>]\n`);
      return 2;
    }
  }
  if (!["local", "fast", "pre-pr"].includes(scope)) {
    write(`Unknown scope: ${scope}\nUsage: pnpm gate [--scope local|fast|pre-pr] [--checks <JSON file>]\n`);
    return 2;
  }

  let focusedChecks = [];
  if (scope === "local") {
    if (!checksPath) {
      write("Local scope requires --checks <JSON file> with a nonempty checks array.\n");
      return 2;
    }
    let plan;
    try {
      plan = JSON.parse(readFileSync(resolve(checksPath), "utf8"));
    } catch (error) {
      write(`Cannot read focused checks plan: ${error.message}\n`);
      return 2;
    }
    if (!plan || !Array.isArray(plan.checks) || plan.checks.length === 0 || plan.checks.some((check) =>
      !check || typeof check.name !== "string" || !check.name.trim() || typeof check.command !== "string" || !check.command.trim() ||
      !Array.isArray(check.args) || check.args.some((arg) => typeof arg !== "string")
    )) {
      write("Invalid focused checks plan: expected { checks: [{ name, command, args }] } with a nonempty array of named commands and string arguments.\n");
      return 2;
    }
    focusedChecks = plan.checks.map(({ name, command, args }) => ({ name, command, args }));
  } else if (checksPath) {
    write("--checks is only valid with --scope local.\n");
    return 2;
  }

  const rust = (name, argv) => ({ name, command: "cargo", args: [`+${rustToolchain}`, ...argv] });
  const checks = [
    { name: "git diff --check origin/main...HEAD", command: "git", args: ["diff", "--check", "origin/main...HEAD"], baseGuidance: true },
    { name: "git diff --check", command: "git", args: ["diff", "--check"] },
    { name: "git diff --cached --check", command: "git", args: ["diff", "--cached", "--check"] },
    { name: `Rust toolchain ${rustToolchain}`, command: "rustc", args: [`+${rustToolchain}`, "--version"], verifyRust: true },
    rust("cargo fmt --all -- --check", ["fmt", "--all", "--", "--check"]),
  ];

  if (scope === "fast") {
    checks.push({ name: "pnpm check:frontend", command: "pnpm", args: ["check:frontend"] });
  } else if (scope === "local") {
    checks.push(
      rust("cargo clippy --workspace --all-targets -- -D warnings", ["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]),
      { name: "pnpm check:frontend", command: "pnpm", args: ["check:frontend"] },
      ...focusedChecks.map((check) => ({ ...check, requireTests: check.command === "cargo" && check.args.includes("test") })),
    );
  } else {
    checks.push(
      rust("cargo clippy --workspace --all-targets -- -D warnings", ["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]),
      { name: "cargo test --workspace", command: "cargo", args: [`+${rustToolchain}`, "test", "--workspace", "--", "--test-threads=4"], requireTests: true },
      { name: "pnpm check:frontend", command: "pnpm", args: ["check:frontend"] },
      { name: "pnpm test:desktop:coverage", command: "pnpm", args: ["test:desktop:coverage"] },
      { name: "pnpm test:desktop:browser", command: "pnpm", args: ["test:desktop:browser"] },
      { name: "pnpm test:web", command: "pnpm", args: ["test:web"] },
      { name: "pnpm check:openapi", command: "pnpm", args: ["check:openapi"] },
      { name: "pnpm check:docs", command: "pnpm", args: ["check:docs"] },
      { name: "pnpm test:scripts", command: "pnpm", args: ["test:scripts"] },
      { name: "pnpm check:deployment", command: "pnpm", args: ["check:deployment"] },
    );
  }

  let failedChecks = 0;
  for (const check of checks) {
    write(`\n=== ${check.name} ===\n`);
    const result = spawn(check.command, check.args, {
      cwd: root,
      encoding: "utf8",
      env: { ...process.env, RUSTUP_TOOLCHAIN: rustToolchain },
      maxBuffer: 64 * 1024 * 1024,
    });
    const output = `${result.stdout ?? ""}${result.stderr ?? ""}`;
    if (output) write(output);

    let status = result.status;
    let detail = result.error?.message ?? (status === null ? "terminated by signal" : `exit ${status}`);
    if (result.error) status = 127;
    if (check.verifyRust && status === 0 && !new RegExp(`^rustc ${rustToolchain.replaceAll(".", "\\.")}\\b`, "m").test(output)) {
      status = 1;
      detail = `rustc did not report the required ${rustToolchain} toolchain`;
    }
    if (check.requireTests && status === 0) {
      const summaries = [...output.matchAll(/test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;/g)];
      const observed = summaries.reduce((sum, match) => sum + Number(match[1]) + Number(match[2]), 0);
      if (summaries.length === 0 || observed === 0) {
        status = 1;
        detail = "cargo test exited 0 without reporting any executed tests";
      }
    }
    if (check.baseGuidance && status !== 0) {
      if (/unknown revision|ambiguous argument|bad revision|not a valid object name/i.test(output)) {
        write("origin/main is unavailable; run `git fetch origin main` and rerun the gate.\n");
      } else {
        write("Committed-candidate whitespace check failed; fix the trailing-whitespace lines shown above.\n");
      }
    }
    if (status === 0) {
      write(`RESULT ${check.name}: PASS (${detail})\n`);
    } else {
      failedChecks += 1;
      write(`RESULT ${check.name}: FAIL (${detail})\n`);
    }
  }

  write(`\nGate summary (${scope}): ${checks.length - failedChecks} passed, ${failedChecks} failed.\n`);
  return failedChecks === 0 ? 0 : 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = runGate();
}
