import { spawn } from "node:child_process";
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

const server = spawn("cargo", ["run", "--quiet", "-p", "rcdvlog-server"], {
  cwd: repository,
  env: {
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
  },
  stdio: "inherit",
});

function stop(signal = "SIGTERM") {
  if (!server.killed) server.kill(signal);
}

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => stop(signal));
}

server.on("exit", (code) => {
  rmSync(fixture, { recursive: true, force: true });
  process.exit(code ?? 0);
});
