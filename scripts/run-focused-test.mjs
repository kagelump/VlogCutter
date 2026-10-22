#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const ci = readFileSync(resolve(root, ".github/workflows/ci.yml"), "utf8");
const pin = ci.match(/^  RUST_TOOLCHAIN: "([^"]+)"$/m)?.[1];
if (!pin) throw new Error("Cannot determine pinned RUST_TOOLCHAIN from .github/workflows/ci.yml");

export function runFocusedTest({ args = process.argv.slice(2), spawn = spawnSync, write = (value) => process.stdout.write(value) } = {}) {
  if (args.length !== 2 || !args[0] || !args[1] || /\s/.test(args[0]) || /\s/.test(args[1])) {
    write("Usage: pnpm test:focused <cargo-package> <exact-rust-test-name>\n");
    return 2;
  }
  const [packageName, testName] = args;
  const commandArgs = [`+${pin}`, "test", "-p", packageName, testName, "--", "--exact"];
  write(`=== cargo ${commandArgs.join(" ")} ===\n`);
  const result = spawn("cargo", commandArgs, {
    cwd: root,
    encoding: "utf8",
    env: { ...process.env, RUSTUP_TOOLCHAIN: pin },
    maxBuffer: 64 * 1024 * 1024,
  });
  const output = `${result.stdout ?? ""}${result.stderr ?? ""}`;
  if (output) write(output);
  if (result.error || result.status !== 0) {
    write(`RESULT focused Rust test: FAIL (${result.error?.message ?? `exit ${result.status ?? "signal"}`})\n`);
    return result.error ? 127 : (result.status ?? 1);
  }

  const escapedName = testName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const namedResults = [...output.matchAll(new RegExp(`^test ${escapedName} \\.\\.\\. (ok|FAILED)$`, "gm"))];
  const summaries = [...output.matchAll(/test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;/g)];
  const executed = summaries.reduce((sum, match) => sum + Number(match[1]) + Number(match[2]), 0);
  if (namedResults.length === 0 || namedResults.every((match) => match[1] !== "ok") || executed < 1) {
    write(`RESULT focused Rust test: FAIL (expected test ${testName}; observed ${namedResults.length} matching result(s), ${executed} executed test(s))\n`);
    return 1;
  }
  write(`RESULT focused Rust test: PASS (observed ${namedResults.length} result(s) named ${testName}; ${executed} executed test(s))\n`);
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = runFocusedTest();
}
