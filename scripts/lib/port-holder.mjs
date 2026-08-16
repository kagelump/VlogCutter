// Who is holding a pinned dev port, and whether it is one of ours.
//
// The desktop dev server's 1420 is a *contract*: `tauri.conf.json`'s `devUrl`,
// the Playwright base URL, and the media origin's `ALLOWED_ORIGINS` all name
// it, and an origin-check mismatch fails silently — the media simply never
// paints. So the port cannot float; only the failure can be made legible.
//
// Nothing here kills anything. A process this cannot positively identify as
// this repository's own dev server gets named and left alone: the alternative
// is a script that occasionally kills a database because it happened to be
// listening on a port a developer wanted.
//
// Run the tests with: node --test "scripts/*.test.mjs"

import { execFileSync } from "node:child_process";
import { readFileSync, readlinkSync } from "node:fs";
import { createServer } from "node:net";

/** Is `port` free to bind on loopback right now? */
export function portIsFree(port) {
  return new Promise((resolve, reject) => {
    const probe = createServer();
    probe.on("error", (error) => (error.code === "EADDRINUSE" ? resolve(false) : reject(error)));
    probe.listen(port, "127.0.0.1", () => probe.close(() => resolve(true)));
  });
}

/**
 * The pid/command pairs in `lsof -nP -iTCP:<port> -sTCP:LISTEN` output.
 *
 * lsof's default columnar output is parsed rather than its `-F` machine format
 * because the columns are what a developer sees when they run it themselves,
 * and the two staying the same is worth more here than strictness.
 */
export function parseLsof(output) {
  const holders = [];
  for (const line of output.split("\n").slice(1)) {
    const [command, pid] = line.trim().split(/\s+/);
    if (!command || !/^\d+$/.test(pid ?? "")) continue;
    if (holders.some((holder) => holder.pid === Number(pid))) continue;
    holders.push({ command, pid: Number(pid) });
  }
  return holders;
}

/** The processes listening on `port`, or `[]` when nothing can be determined. */
export function findHolders(port, run = defaultRun) {
  try {
    return parseLsof(run("lsof", ["-nP", `-iTCP:${port}`, "-sTCP:LISTEN"]));
  } catch {
    // lsof exits non-zero when nothing matches, and may not be installed at
    // all. Neither is a reason to fail: the port is held either way, and an
    // unnamed holder is still better reported than a raw EADDRINUSE.
    return [];
  }
}

function defaultRun(file, args) {
  return execFileSync(file, args, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
}

/**
 * The full command line of `pid`, or "" when it cannot be read.
 *
 * Collapsed to one line and bounded: this goes into a message a developer
 * reads at a glance, and `node -e` with a script in it would otherwise bury
 * the sentence that matters underneath it.
 */
export function commandLineOf(pid, run = defaultRun) {
  return oneLine(readCommandLine(pid, run));
}

const COMMAND_LINE_LIMIT = 120;

function oneLine(value) {
  const collapsed = value.replace(/\s+/g, " ").trim();
  return collapsed.length > COMMAND_LINE_LIMIT ? `${collapsed.slice(0, COMMAND_LINE_LIMIT - 1)}…` : collapsed;
}

function readCommandLine(pid, run) {
  try {
    // Linux exposes it directly; `ps` covers macOS, where the tests will not
    // run but the developers will.
    return readFileSync(`/proc/${pid}/cmdline`, "utf8").replaceAll("\0", " ");
  } catch {
    try {
      return run("ps", ["-o", "command=", "-p", String(pid)]);
    } catch {
      return "";
    }
  }
}

/**
 * The working directory of `pid`, or "" when it cannot be read.
 *
 * This is what identifies a process as ours, not its argv: pnpm launches Vite
 * as `node ./node_modules/.bin/../vite/bin/vite.js`, with no absolute path in
 * the command line at all.
 */
export function workingDirectoryOf(pid, run = defaultRun) {
  try {
    return readlinkSync(`/proc/${pid}/cwd`);
  } catch {
    try {
      // macOS has no /proc; lsof's field output names the cwd on an `n` line.
      const lines = run("lsof", ["-a", "-d", "cwd", "-p", String(pid), "-Fn"]).split("\n");
      return lines.filter((line) => line.startsWith("n")).at(-1)?.slice(1).trim() ?? "";
    } catch {
      return "";
    }
  }
}

/**
 * Is this process this repository's own Vite dev server?
 *
 * Both halves are required: something running `vite`, *and* running it from
 * inside this checkout. A Vite in another project is somebody else's process
 * and gets named, not offered up for killing.
 */
export function isOurDevServer({ commandLine, workingDirectory }, repositoryRoot) {
  if (!commandLine || !workingDirectory || !repositoryRoot) return false;
  if (!/\bvite\b/.test(commandLine)) return false;
  return workingDirectory === repositoryRoot || workingDirectory.startsWith(`${repositoryRoot}/`);
}

/**
 * What to tell the developer about a held port.
 *
 * Returns `{ ok }` when the port is free, and otherwise a `message` and
 * whether the holder was positively identified as ours.
 */
export function diagnose({ port, purpose, holders, repositoryRoot, commandLines = {}, workingDirectories = {} }) {
  if (!holders.length) {
    return {
      ok: false,
      ours: false,
      message: [
        `Port ${port} is already in use, and ${purpose} needs exactly that port.`,
        "Could not determine which process is holding it (is lsof available?).",
        `Try: lsof -nP -iTCP:${port} -sTCP:LISTEN`,
      ].join("\n"),
    };
  }

  const lines = [`Port ${port} is held, and ${purpose} needs exactly that port.`];
  let ours = false;
  for (const holder of holders) {
    const commandLine = commandLines[holder.pid] ?? "";
    const workingDirectory = workingDirectories[holder.pid] ?? "";
    if (isOurDevServer({ commandLine, workingDirectory }, repositoryRoot)) {
      ours = true;
      lines.push(`  ${holder.command} (pid ${holder.pid}) — this repository's dev server, left over from an earlier run.`);
      lines.push(`  Stop it with:  kill ${holder.pid}`);
    } else {
      lines.push(`  ${holder.command} (pid ${holder.pid})${commandLine ? ` — ${commandLine}` : ""}`);
    }
  }
  if (!ours) {
    // Naming it is the whole job. Killing a process this cannot identify is
    // how a preflight ends up taking down someone's database.
    lines.push("  Not recognised as this repository's dev server; stop it yourself if it should not be there.");
  }
  return { ok: false, ours, message: lines.join("\n") };
}
