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
  let scope = "pre-pr";
  for (let i = 0; i < args.length; i += 1) {
    if (args[i] === "--scope" && args[i + 1]) {
      scope = args[++i];
    } else {
      write(`Unknown argument: ${args[i]}\nUsage: pnpm gate [--scope fast|pre-pr]\n`);
      return 2;
    }
  }
  if (!["fast", "pre-pr"].includes(scope)) {
    write(`Unknown scope: ${scope}\nUsage: pnpm gate [--scope fast|pre-pr]\n`);
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
  } else {
    checks.push(
      rust("cargo clippy --workspace --all-targets -- -D warnings", ["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]),
      { name: "cargo test --workspace", command: "cargo", args: [`+${rustToolchain}`, "test", "--workspace"], requireTests: true },
      { name: "pnpm check:frontend", command: "pnpm", args: ["check:frontend"] },
      { name: "pnpm test:desktop", command: "pnpm", args: ["test:desktop"] },
      { name: "pnpm test:desktop:cuj", command: "pnpm", args: ["test:desktop:cuj"] },
      { name: "pnpm test:desktop:coverage", command: "pnpm", args: ["test:desktop:coverage"] },
      { name: "pnpm test:desktop:browser", command: "pnpm", args: ["test:desktop:browser"] },
      { name: "pnpm test:web", command: "pnpm", args: ["test:web"] },
      { name: "pnpm test:integration", command: "pnpm", args: ["test:integration"] },
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
