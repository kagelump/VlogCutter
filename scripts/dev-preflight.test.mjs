import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { findOnPath, parseMajorMinor, readPins, runPreflight } from "./dev-preflight.mjs";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));
const temps = [];
after(() => temps.forEach((dir) => rmSync(dir, { recursive: true, force: true })));

function tempDir() {
  const dir = mkdtempSync(join(tmpdir(), "vlogcutter-preflight-"));
  temps.push(dir);
  return dir;
}

function fakeRoot({ node = "24.11.0", minimum = "(6, 1)" } = {}) {
  const root = tempDir();
  mkdirSync(join(root, ".github/workflows"), { recursive: true });
  mkdirSync(join(root, "core/rcdvlog-core/src"), { recursive: true });
  writeFileSync(join(root, ".github/workflows/ci.yml"), `env:\n  RUST_TOOLCHAIN: "1.97.1"\n  NODE_VERSION: "${node}"\n  PNPM_VERSION: "10.17.0"\n  NEXTEST_VERSION: "0.9.151"\n`);
  writeFileSync(join(root, "core/rcdvlog-core/src/prerequisites.rs"), `pub const MINIMUM_FFMPEG: (u32, u32) = ${minimum};\n`);
  return root;
}

function executable(dir, name, body) {
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, name), `#!/bin/sh\n${body}\n`, { mode: 0o755 });
}

function ffmpegInstall(dir, { version = "7.1.3", encoders = ["libx264", "aac"] } = {}) {
  const lines = encoders.map((encoder) => ` ${encoder === "aac" ? "A" : "V"}....D ${encoder}  fixture encoder`).join("\\n");
  executable(dir, "ffmpeg", `case "$2" in\n  -version) echo "ffmpeg version ${version} Copyright";;\n  -encoders) printf ' ------\\n${lines}\\n';;\nesac`);
  executable(dir, "ffprobe", `echo "ffprobe version ${version} Copyright"`);
}

function fakeCargo({ version = "1.97.1", nextest = 'echo "cargo-nextest 0.9.151 (fake)"' } = {}) {
  return `case "$2" in\n  --version) echo "cargo ${version} (fake)";;\n  nextest) ${nextest};;\n  *) exit 1;;\nesac`;
}

/** A complete passing toolchain; `overrides` replace or (with null) remove tools. */
function toolchain(overrides = {}) {
  const bin = join(tempDir(), "bin");
  const tools = {
    pnpm: 'echo "10.17.0"',
    rustc: 'echo "rustc 1.97.1 (fake)"',
    cargo: fakeCargo(),
    exiftool: 'echo "13.36"',
    ...overrides,
  };
  for (const [name, body] of Object.entries(tools)) if (body !== null) executable(bin, name, body);
  return bin;
}

function preflight({ root = fakeRoot(), path, nodeVersion = "24.11.0", skip = [] }) {
  let output = "";
  const result = runPreflight({ root, env: { PATH: path }, nodeVersion, platform: "darwin", skip, write: (chunk) => { output += chunk; } });
  const level = (name) => result.results.find((entry) => entry.name === name)?.level;
  return { ...result, output, level };
}

test("a complete pinned toolchain passes", () => {
  const ffmpeg = join(tempDir(), "ffmpeg");
  ffmpegInstall(ffmpeg);
  const result = preflight({ path: `${toolchain()}:${ffmpeg}` });
  assert.equal(result.ok, true, result.output);
  assert.match(result.output, /Preflight: 7 passed, 0 failed, 0 warnings\./);
});

test("a shadowed ffmpeg without libx264 fails and names the install that would pass", () => {
  const conda = join(tempDir(), "miniconda3", "bin");
  const brew = join(tempDir(), "ffmpeg@7", "bin");
  ffmpegInstall(conda, { version: "6.1.1", encoders: ["libopenh264", "aac"] });
  ffmpegInstall(brew, { version: "7.1.3" });
  const result = preflight({ path: `${toolchain()}:${conda}:${brew}` });
  assert.equal(result.ok, false);
  assert.equal(result.level("ffmpeg"), "fail");
  assert.match(result.output, /miniconda3\/bin\/ffmpeg 6\.1\.1: missing encoder libx264/);
  assert.ok(result.output.includes(`passes but is shadowed; put ${brew} before ${conda} on PATH`), result.output);
  assert.match(result.output, /conda deactivate/);
});

test("ffmpeg below the product minimum fails; Ubuntu 24.04's 6.1.1 passes", () => {
  const old = join(tempDir(), "old");
  ffmpegInstall(old, { version: "6.0.1" });
  const tools = toolchain();
  const rejected = preflight({ path: `${tools}:${old}` });
  assert.equal(rejected.level("ffmpeg"), "fail");
  assert.match(rejected.output, /6\.0\.1: older than the required minimum 6\.1/);
  assert.equal(rejected.level("ffprobe"), "fail");

  const ubuntu = join(tempDir(), "ubuntu");
  ffmpegInstall(ubuntu, { version: "6.1.1-3ubuntu5" });
  const accepted = preflight({ path: `${tools}:${ubuntu}` });
  assert.equal(accepted.ok, true, accepted.output);
});

test("the ffmpeg floor comes from the Rust constant, not a copy", () => {
  const ffmpeg = join(tempDir(), "ffmpeg");
  ffmpegInstall(ffmpeg, { version: "7.1.3" });
  const result = preflight({ root: fakeRoot({ minimum: "(8, 0)" }), path: `${toolchain()}:${ffmpeg}` });
  assert.equal(result.level("ffmpeg"), "fail");
  assert.match(result.output, /older than the required minimum 8\.0/);
});

test("missing or mismatched cargo-nextest fails with the pinned install command", () => {
  const ffmpeg = join(tempDir(), "ffmpeg");
  ffmpegInstall(ffmpeg);
  const missing = preflight({ path: `${toolchain({ cargo: fakeCargo({ nextest: 'echo "error: no such command: \\`nextest\\`" >&2; exit 101' }) })}:${ffmpeg}` });
  assert.equal(missing.level("nextest"), "fail");
  assert.match(missing.output, /cargo-nextest is not installed \(CI pins 0\.9\.151\)/);
  assert.match(missing.output, /cargo install --locked cargo-nextest@0\.9\.151/);

  const stale = preflight({ path: `${toolchain({ cargo: fakeCargo({ nextest: 'echo "cargo-nextest 0.9.100 (fake)"' }) })}:${ffmpeg}` });
  assert.equal(stale.level("nextest"), "fail");
  assert.match(stale.output, /0\.9\.100, but CI pins 0\.9\.151/);
});

test("wrong node, pnpm, or rust versions fail", () => {
  const ffmpeg = join(tempDir(), "ffmpeg");
  ffmpegInstall(ffmpeg);
  const result = preflight({
    nodeVersion: "22.0.0",
    path: `${toolchain({ pnpm: 'echo "9.0.0"', cargo: fakeCargo({ version: "1.85.0" }) })}:${ffmpeg}`,
  });
  assert.deepEqual(["node", "pnpm", "rust"].map(result.level), ["fail", "fail", "fail"]);
  assert.match(result.output, /22\.0\.0, but CI pins 24\.11\.0/);
});

test("a non-rustup cargo first on PATH fails the rust check, even if rustc matches", () => {
  const ffmpeg = join(tempDir(), "ffmpeg");
  ffmpegInstall(ffmpeg);
  // Homebrew's cargo rejects `+<toolchain>`; its rustc of the same version ignores it.
  const homebrew = join(tempDir(), "homebrew", "bin");
  executable(homebrew, "cargo", 'echo "error: no such command: \\`+1.97.1\\`" >&2; exit 101');
  executable(homebrew, "rustc", 'echo "rustc 1.97.1 (Homebrew)"');
  const result = preflight({ path: `${homebrew}:${toolchain()}:${ffmpeg}` });
  assert.equal(result.level("rust"), "fail");
  assert.ok(result.output.includes(`${join(homebrew, "cargo")} is not rustup's proxy`), result.output);
  assert.ok(result.output.includes(`before ${homebrew} on PATH`), result.output);
  assert.equal(result.level("nextest"), "fail");
  assert.match(result.output, /nextest: not checked/);
});

test("a rustc from another install than cargo only warns", () => {
  const ffmpeg = join(tempDir(), "ffmpeg");
  ffmpegInstall(ffmpeg);
  const other = join(tempDir(), "other");
  executable(other, "rustc", 'echo "rustc 1.97.1 (Homebrew)"');
  const result = preflight({ path: `${other}:${toolchain()}:${ffmpeg}` });
  assert.equal(result.ok, true, result.output);
  assert.equal(result.level("rust"), "warn");
});

test("missing exiftool only warns; --skip skips a check", () => {
  const ffmpeg = join(tempDir(), "ffmpeg");
  ffmpegInstall(ffmpeg);
  const result = preflight({ path: `${toolchain({ exiftool: null, cargo: fakeCargo({ nextest: "exit 101" }) })}:${ffmpeg}`, skip: ["nextest"] });
  assert.equal(result.ok, true, result.output);
  assert.equal(result.level("exiftool"), "warn");
  assert.equal(result.level("nextest"), "skip");
  assert.match(result.output, /1 warnings, 1 skipped/);
});

test("PATH lookup keeps order and collapses symlinked duplicates", () => {
  const a = join(tempDir(), "a");
  const b = join(tempDir(), "b");
  ffmpegInstall(a);
  mkdirSync(b);
  symlinkSync(join(a, "ffmpeg"), join(b, "ffmpeg"));
  assert.deepEqual(findOnPath("ffmpeg", `${b}:${a}`), [join(b, "ffmpeg")]);
});

test("version parsing matches the product's major.minor rules", () => {
  assert.deepEqual(parseMajorMinor("6.1.1-3ubuntu5"), [6, 1]);
  assert.deepEqual(parseMajorMinor("7"), [7, 0]);
  assert.deepEqual(parseMajorMinor("9.0.2"), [9, 0]);
  assert.equal(parseMajorMinor("N-112345-gabcdef"), null);
});

test("pins are read from ci.yml and prerequisites.rs in this repository", () => {
  const pins = readPins(repoRoot);
  assert.match(pins.rust, /^\d+\.\d+\.\d+$/);
  assert.match(pins.nextest, /^\d+\.\d+\.\d+$/);
  assert.equal(pins.ffmpeg.length, 2);
  assert.ok(pins.ffmpeg.every(Number.isInteger));
});

test("CLI runs through a symlinked path and rejects unknown arguments", () => {
  const link = join(tempDir(), "scripts-link");
  symlinkSync(join(repoRoot, "scripts"), link);
  const bad = spawnSync(process.execPath, [join(link, "dev-preflight.mjs"), "--skip", "bogus"], { encoding: "utf8" });
  assert.equal(bad.status, 2);
  assert.match(bad.stdout, /Unknown check for --skip: bogus/);
});
