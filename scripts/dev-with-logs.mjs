// Runs `tauri dev` and tees its combined output to ./logs/dev-<timestamp>.log,
// keeping ./logs bounded to TOTAL_CAP_BYTES (oldest-first). Logging never blocks
// the dev server: any fs failure degrades to plain passthrough.
import { spawn } from "node:child_process";
import { createWriteStream, existsSync, mkdirSync, readdirSync, rmSync, statSync } from "node:fs";
import { resolve } from "node:path";
import {
  FILE_CAP_BYTES,
  TOTAL_CAP_BYTES,
  planPrune,
  sessionFileName,
} from "./lib/dev-log-rotation.mjs";

const repoRoot = resolve(import.meta.dirname, "..");
const logsDir = resolve(repoRoot, "logs");

function listLogFiles() {
  return readdirSync(logsDir)
    .filter((name) => name.startsWith("dev-") && name.endsWith(".log"))
    .map((name) => {
      const { size, mtimeMs } = statSync(resolve(logsDir, name));
      return { name, size, mtimeMs };
    });
}

function prune() {
  for (const name of planPrune(listLogFiles(), TOTAL_CAP_BYTES)) {
    try {
      rmSync(resolve(logsDir, name));
    } catch {
      // A file we cannot delete is not worth aborting the dev server over.
    }
  }
}

// Best-effort file sink. Returns { write, close } that no-op if logging is unavailable.
function openSink() {
  try {
    if (!existsSync(logsDir)) mkdirSync(logsDir, { recursive: true });
    prune();
    let current = createWriteStream(resolve(logsDir, sessionFileName()), { flags: "a" });
    let bytes = 0;
    return {
      write(chunk) {
        try {
          current.write(chunk);
          bytes += chunk.length;
          if (bytes >= FILE_CAP_BYTES) {
            current.end();
            prune();
            current = createWriteStream(resolve(logsDir, sessionFileName()), { flags: "a" });
            bytes = 0;
          }
        } catch {
          // Ignore write errors; the terminal still shows output.
        }
      },
      close() {
        try {
          current.end();
        } catch {
          // already closed
        }
      },
    };
  } catch (error) {
    process.stderr.write(`[dev-logs] logging disabled: ${error.message}\n`);
    return { write() {}, close() {} };
  }
}

const sink = openSink();

const child = spawn("pnpm", ["--dir", "apps/desktop", "tauri", "dev"], {
  cwd: repoRoot,
  env: process.env,
  stdio: ["inherit", "pipe", "pipe"],
});

function tee(source, target) {
  source.on("data", (chunk) => {
    target.write(chunk);
    sink.write(chunk);
  });
}
tee(child.stdout, process.stdout);
tee(child.stderr, process.stderr);

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => child.kill(signal));
}

child.on("exit", (code, signal) => {
  sink.close();
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 0);
  }
});

child.on("error", (error) => {
  process.stderr.write(`[dev-logs] failed to start tauri dev: ${error.message}\n`);
  sink.close();
  process.exit(1);
});
