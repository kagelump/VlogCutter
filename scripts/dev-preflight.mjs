#!/usr/bin/env node
// Preflight for local development, the gate, and CI: verifies the pinned
// toolchain and the media tools the tests shell out to, in about a second,
// before anything slow runs. Every failure names the exact fix.
//
// Pins are read from their single sources rather than restated here:
// - Rust, Node, pnpm, cargo-nextest: the `env:` block of .github/workflows/ci.yml
// - FFmpeg minimum: `MINIMUM_FFMPEG` in core/rcdvlog-core/src/prerequisites.rs

import { accessSync, constants, readFileSync, realpathSync, statSync } from "node:fs";
import { delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const defaultRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const CHECK_NAMES = ["node", "pnpm", "rust", "nextest", "ffmpeg", "ffprobe", "exiftool"];
const REQUIRED_ENCODERS = [
  ["V", "libx264"],
  ["A", "aac"],
];

export function readPins(root = defaultRoot) {
  const ci = readFileSync(resolve(root, ".github/workflows/ci.yml"), "utf8");
  const pin = (key) => {
    const match = ci.match(new RegExp(`^  ${key}: "([^"]+)"$`, "m"));
    if (!match) throw new Error(`Cannot determine pinned ${key} from .github/workflows/ci.yml`);
    return match[1];
  };
  const prerequisites = readFileSync(resolve(root, "core/rcdvlog-core/src/prerequisites.rs"), "utf8");
  const ffmpeg = prerequisites.match(/^pub const MINIMUM_FFMPEG: \(u32, u32\) = \((\d+), (\d+)\);$/m);
  if (!ffmpeg) throw new Error("Cannot determine MINIMUM_FFMPEG from core/rcdvlog-core/src/prerequisites.rs");
  return {
    rust: pin("RUST_TOOLCHAIN"),
    node: pin("NODE_VERSION"),
    pnpm: pin("PNPM_VERSION"),
    nextest: pin("NEXTEST_VERSION"),
    ffmpeg: [Number(ffmpeg[1]), Number(ffmpeg[2])],
  };
}

/** Every executable named `name` on PATH, in lookup order, deduplicated by real path. */
export function findOnPath(name, pathValue) {
  const seen = new Set();
  const found = [];
  for (const directory of (pathValue ?? "").split(delimiter)) {
    if (!directory) continue;
    const candidate = join(directory, name);
    try {
      if (!statSync(candidate).isFile()) continue;
      accessSync(candidate, constants.X_OK);
      const real = realpathSync(candidate);
      if (seen.has(real)) continue;
      seen.add(real);
      found.push(candidate);
    } catch {
      // Not present or not executable in this directory.
    }
  }
  return found;
}

/** Mirrors the product's parsing: "6.1.1-3ubuntu5" -> [6, 1]; "7" -> [7, 0]. */
export function parseMajorMinor(version) {
  const parts = String(version).split(/[.-]/);
  if (!/^\d+$/.test(parts[0] ?? "")) return null;
  const minor = parts.length > 1 ? parts[1] : "0";
  if (!/^\d+$/.test(minor)) return null;
  return [Number(parts[0]), Number(minor)];
}

const atLeast = ([major, minor], [minMajor, minMinor]) => major > minMajor || (major === minMajor && minor >= minMinor);

export function runPreflight({
  root = defaultRoot,
  env = process.env,
  nodeVersion = process.versions.node,
  platform = process.platform,
  skip = [],
  write = (value) => process.stdout.write(value),
} = {}) {
  const pins = readPins(root);
  const run = (command, args) => {
    const result = spawnSync(command, args, { cwd: root, env, encoding: "utf8", timeout: 15_000, maxBuffer: 8 * 1024 * 1024 });
    return { ok: !result.error && result.status === 0, output: `${result.stdout ?? ""}${result.stderr ?? ""}`, error: result.error };
  };
  const minimum = pins.ffmpeg.join(".");
  const results = [];
  const record = (name, level, detail, fix) => results.push({ name, level, detail, fix });

  const checks = {
    node() {
      if (nodeVersion === pins.node) return record("node", "pass", nodeVersion);
      record("node", "fail", `${nodeVersion}, but CI pins ${pins.node}`, `Install Node ${pins.node} (e.g. \`nvm install ${pins.node} && nvm use ${pins.node}\`).`);
    },
    pnpm() {
      const result = run("pnpm", ["--version"]);
      const version = result.output.trim().split(/\s+/).pop();
      if (result.ok && version === pins.pnpm) return record("pnpm", "pass", version);
      record("pnpm", "fail", result.ok ? `${version}, but CI pins ${pins.pnpm}` : "not found on PATH", `Install pnpm ${pins.pnpm} (\`corepack enable && corepack prepare pnpm@${pins.pnpm} --activate\`).`);
    },
    rust() {
      // The gate runs `cargo +<pin> ...`, which only rustup's proxy understands.
      // Check cargo itself: a Homebrew rustc of the same version ignores `+<pin>`
      // and would pass a rustc-only check while every gate cargo command fails.
      const install = `\`rustup toolchain install ${pins.rust} --component rustfmt clippy\` (rustup is required; source ~/.cargo/env if cargo is missing).`;
      const [cargo] = findOnPath("cargo", env.PATH);
      if (!cargo) return record("rust", "fail", "cargo not found on PATH", install);
      const result = run("cargo", [`+${pins.rust}`, "--version"]);
      if (/no such (?:command|subcommand): `?\+/.test(result.output)) {
        return record("rust", "fail", `${cargo} is not rustup's proxy, so \`cargo +${pins.rust}\` fails`, `Put rustup's bin directory (usually ~/.cargo/bin) before ${dirname(cargo)} on PATH, or uninstall the non-rustup Rust (e.g. \`brew uninstall rust\`).`);
      }
      if (!result.ok) return record("rust", "fail", `cargo +${pins.rust} is unavailable`, install);
      if (!new RegExp(`^cargo ${pins.rust.replaceAll(".", "\\.")}\\b`, "m").test(result.output)) {
        return record("rust", "fail", `cargo +${pins.rust} reported: ${result.output.trim().split("\n")[0]}`, install);
      }
      const [rustc] = findOnPath("rustc", env.PATH);
      if (rustc && dirname(rustc) !== dirname(cargo)) {
        return record("rust", "warn", `${pins.rust} via ${cargo}, but rustc resolves to ${rustc}`, `Put ${dirname(cargo)} before ${dirname(rustc)} on PATH so tools that call rustc directly use the pin.`);
      }
      record("rust", "pass", `${pins.rust} via ${cargo}`);
    },
    nextest() {
      if (results.find((result) => result.name === "rust")?.level === "fail") {
        return record("nextest", "fail", "not checked: `cargo +<pin>` is unusable (see rust above)", "Fix the rust check first.");
      }
      const result = run("cargo", [`+${pins.rust}`, "nextest", "--version"]);
      const version = result.output.match(/nextest\S* (\d+\.\d+\.\d+)/)?.[1];
      const fix = `\`cargo install --locked cargo-nextest@${pins.nextest}\``;
      if (!result.ok || !version) return record("nextest", "fail", `cargo-nextest is not installed (CI pins ${pins.nextest})`, fix);
      if (version !== pins.nextest) return record("nextest", "fail", `${version}, but CI pins ${pins.nextest}`, fix);
      record("nextest", "pass", version);
    },
    ffmpeg() {
      const candidates = findOnPath("ffmpeg", env.PATH);
      if (candidates.length === 0) {
        return record("ffmpeg", "fail", "not found on PATH", platform === "darwin" ? "`brew install ffmpeg`" : "Install your distribution's ffmpeg package (it must include libx264).");
      }
      const inspected = candidates.map((path) => inspectFfmpeg(path, pins.ffmpeg, run));
      const [first] = inspected;
      if (first.problems.length === 0) return record("ffmpeg", "pass", `${first.version} at ${first.path}`);
      const usable = inspected.find((candidate) => candidate.problems.length === 0);
      let fix = platform === "darwin"
        ? `Install a build with libx264 and aac, ${minimum} or newer (\`brew install ffmpeg\`).`
        : `Install an ffmpeg ${minimum}+ build with libx264 and aac.`;
      if (usable) {
        fix = `${usable.path} (${usable.version}) passes but is shadowed; put ${dirname(usable.path)} before ${dirname(first.path)} on PATH.`;
      }
      if (/conda/i.test(first.path)) fix += " This ffmpeg comes from conda: `conda deactivate`, or stop conda's base environment from auto-activating, in shells used for this repository.";
      record("ffmpeg", "fail", `${first.path} ${first.version ?? "(unknown version)"}: ${first.problems.join("; ")}`, fix);
    },
    ffprobe() {
      const [path] = findOnPath("ffprobe", env.PATH);
      if (!path) return record("ffprobe", "fail", "not found on PATH", "ffprobe ships with ffmpeg; install it alongside the ffmpeg above.");
      const result = run(path, ["-hide_banner", "-version"]);
      const version = result.output.match(/^ffprobe version (\S+)/m)?.[1];
      const parsed = version && parseMajorMinor(version);
      if (!result.ok || !parsed) return record("ffprobe", "fail", `${path}: no recognizable version`, "Reinstall ffmpeg/ffprobe.");
      if (!atLeast(parsed, pins.ffmpeg)) return record("ffprobe", "fail", `${path} ${version} is older than ${minimum}`, "Put a newer ffprobe (from the same install as ffmpeg) first on PATH.");
      const [ffmpeg] = findOnPath("ffmpeg", env.PATH);
      if (ffmpeg && dirname(ffmpeg) !== dirname(path)) {
        return record("ffprobe", "warn", `${version} at ${path} is from a different install than ${ffmpeg}`, "Keep ffmpeg and ffprobe from one install so probing and encoding agree.");
      }
      record("ffprobe", "pass", `${version} at ${path}`);
    },
    exiftool() {
      const result = run("exiftool", ["-ver"]);
      if (result.ok) return record("exiftool", "pass", result.output.trim());
      record("exiftool", "warn", "not found on PATH; ingest metadata needs it at runtime, the tests do not", platform === "darwin" ? "`brew install exiftool`" : "Install your distribution's exiftool package (libimage-exiftool-perl).");
    },
  };

  for (const name of CHECK_NAMES) {
    if (skip.includes(name)) {
      record(name, "skip", "skipped by --skip");
      continue;
    }
    checks[name]();
  }

  const labels = { pass: "PASS", fail: "FAIL", warn: "WARN", skip: "SKIP" };
  write("=== preflight (pinned toolchain and media tools) ===\n");
  for (const result of results) {
    write(`${labels[result.level]} ${result.name}: ${result.detail}\n`);
    if (result.fix) write(`     fix: ${result.fix}\n`);
  }
  const count = (level) => results.filter((result) => result.level === level).length;
  const failed = count("fail");
  write(`Preflight: ${count("pass")} passed, ${failed} failed, ${count("warn")} warnings${count("skip") ? `, ${count("skip")} skipped` : ""}.\n`);
  return { ok: failed === 0, results };
}

function inspectFfmpeg(path, minimum, run) {
  const problems = [];
  const versionResult = run(path, ["-hide_banner", "-version"]);
  const version = versionResult.output.match(/^ffmpeg version (\S+)/m)?.[1] ?? null;
  const parsed = version && parseMajorMinor(version);
  if (!versionResult.ok || !parsed) {
    problems.push("no recognizable version (the product rejects it too)");
  } else if (!atLeast(parsed, minimum)) {
    problems.push(`older than the required minimum ${minimum.join(".")}`);
  }
  const encoders = run(path, ["-hide_banner", "-encoders"]).output;
  for (const [kind, encoder] of REQUIRED_ENCODERS) {
    if (!new RegExp(`^\\s*${kind}\\S*\\s+${encoder}\\s`, "m").test(encoders)) problems.push(`missing encoder ${encoder}`);
  }
  return { path, version, problems };
}

function parseArgs(args) {
  const skip = [];
  for (let i = 0; i < args.length; i += 1) {
    if (args[i] === "--skip" && args[i + 1]) {
      const name = args[++i];
      if (!CHECK_NAMES.includes(name)) throw new Error(`Unknown check for --skip: ${name} (expected one of ${CHECK_NAMES.join(", ")})`);
      skip.push(name);
    } else {
      throw new Error(`Unknown argument: ${args[i]}\nUsage: pnpm preflight [--skip <${CHECK_NAMES.join("|")}>]...`);
    }
  }
  return { skip };
}

if (import.meta.main) {
  let options;
  try {
    options = parseArgs(process.argv.slice(2));
  } catch (error) {
    process.stdout.write(`${error.message}\n`);
    process.exit(2);
  }
  process.exit(runPreflight(options).ok ? 0 : 1);
}
