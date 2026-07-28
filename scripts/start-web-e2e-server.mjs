import { spawn, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const repository = resolve(import.meta.dirname, "..");
const fixture = mkdtempSync(join(tmpdir(), "rcdvlog-web-e2e-"));
const state = join(fixture, "state");
const temp = join(state, "tmp");
const derived = join(fixture, "derived");
const backup = join(fixture, "backup");
for (const path of [temp, derived, backup]) mkdirSync(path, { recursive: true });

const runtimeEnvironment = {
  ...process.env,
  RCDVLOG_DATA_DIR: state,
  RCDVLOG_TEMP_DIR: temp,
  RCDVLOG_DERIVED_DIR: derived,
  RCDVLOG_BACKUP_DIR: backup,
  RCDVLOG_LISTEN_ADDR: "127.0.0.1:8090",
  RCDVLOG_WEB_DIR: join(repository, "apps/web/dist"),
  RCDVLOG_FFMPEG: "/usr/bin/true",
  RCDVLOG_FFPROBE: "/usr/bin/true",
  RCDVLOG_EXIFTOOL: "/usr/bin/true",
  RUST_LOG: "warn",
};

const seed = spawnSync(
  "cargo",
  ["run", "--quiet", "-p", "rcdvlog-server", "--example", "web_e2e_seed"],
  { cwd: repository, env: runtimeEnvironment, encoding: "utf8" },
);
if (seed.status !== 0) {
  throw new Error(`seed real web project fixture: ${seed.stderr}`);
}

const bootstrapFiles = [1, 2, 3].map((index) => join(tmpdir(), `rcdvlog-web-e2e-bootstrap-${index}.secret`));
for (const bootstrapFile of bootstrapFiles) {
  rmSync(bootstrapFile, { force: true });
  const bootstrap = spawnSync(
    "cargo",
    ["run", "--quiet", "-p", "rcdvlog-server", "--", "admin", "bootstrap", "create", "--output-file", bootstrapFile],
    { cwd: repository, env: runtimeEnvironment, encoding: "utf8" },
  );
  if (bootstrap.status !== 0) {
    throw new Error(`create local bootstrap fixture: ${bootstrap.stderr}`);
  }
}

const server = spawn("cargo", ["run", "--quiet", "-p", "rcdvlog-server"], {
  cwd: repository,
  env: runtimeEnvironment,
  stdio: "inherit",
});

function stop(signal = "SIGTERM") {
  if (!server.killed) server.kill(signal);
  for (const bootstrapFile of bootstrapFiles) rmSync(bootstrapFile, { force: true });
}

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => stop(signal));
}

server.on("exit", (code) => {
  for (const bootstrapFile of bootstrapFiles) rmSync(bootstrapFile, { force: true });
  rmSync(fixture, { recursive: true, force: true });
  process.exit(code ?? 0);
});
