// Where the dev eval bridge is listening, for the two kinds of client.
//
// The bridge binds an ephemeral loopback port (see
// `apps/desktop/src-tauri/src/eval_bridge.rs`), so no client may assume an
// address. A harness that spawned the app reads the marker line off its
// stdout; a shell client that did not reads the address file the app wrote.
// Both paths are here so the runner and `rcdvlog-eval` cannot drift apart.
//
// Run the tests with: node --test "scripts/*.test.mjs"

import { readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

export const COMPILED_MARKER = "RCDVLOG_EVAL_BRIDGE_COMPILED";
export const DEFAULT_ADDRESS_FILE = "rcdvlog-eval-bridge.json";
const MARKER_PREFIX = `[${COMPILED_MARKER}] listening on `;

/** The path the app publishes its address to, honouring the harness override. */
export function addressFilePath(env = process.env) {
  return env.RCDVLOG_EVAL_BRIDGE_ADDRESS_FILE || join(tmpdir(), DEFAULT_ADDRESS_FILE);
}

/** The `ws://…` address in one line of app stdout, or null for any other line. */
export function parseMarkerLine(line) {
  const start = line.indexOf(MARKER_PREFIX);
  if (start === -1) return null;
  const address = line.slice(start + MARKER_PREFIX.length).trim();
  return /^ws:\/\/127\.0\.0\.1:\d{1,5}$/.test(address) ? address : null;
}

/**
 * The address from a stream of app stdout, or null once the stream ends
 * without one. Callers impose the startup timeout — a bridge that never comes
 * up must fail in the harness, where it can be reported, not hang there.
 */
export async function readMarkerAddress(stream) {
  let pending = "";
  for await (const chunk of stream) {
    pending += String(chunk);
    const lines = pending.split("\n");
    pending = lines.pop() ?? "";
    for (const line of lines) {
      const address = parseMarkerLine(line);
      if (address) return address;
    }
  }
  return parseMarkerLine(pending);
}

/**
 * The published address, with the app's liveness resolved.
 *
 * `state` is one of `running`, `stale` (the app that wrote this file is gone),
 * `missing`, or `unreadable`; only `running` carries a usable address.
 */
export function readPublishedAddress(path) {
  let contents;
  try {
    contents = readFileSync(path, "utf8");
  } catch (error) {
    return { state: error.code === "ENOENT" ? "missing" : "unreadable", path };
  }

  let published;
  try {
    published = JSON.parse(contents);
  } catch {
    return { state: "unreadable", path };
  }
  const { address, pid } = published ?? {};
  if (typeof address !== "string" || !Number.isInteger(pid)) {
    return { state: "unreadable", path };
  }
  return { state: isAlive(pid) ? "running" : "stale", address, pid, path };
}

// Signal 0 checks for the process without touching it. EPERM means it exists
// and belongs to someone else, which still answers the question being asked.
function isAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code === "EPERM";
  }
}

/**
 * The address a shell client should connect to, in precedence order: an
 * explicit `--port`, then the `RCDVLOG_EVAL_BRIDGE_PORT` override the app was
 * pinned with, then the address the running app published.
 *
 * `waitMs` polls for the address file, for a caller that starts the app and
 * the client together. Throws with an actionable message when there is
 * nothing to connect to.
 */
export async function resolveAddress({ env = process.env, port, waitMs = 0 } = {}) {
  if (port !== undefined) return { address: `ws://127.0.0.1:${port}`, source: "--port" };
  const override = env.RCDVLOG_EVAL_BRIDGE_PORT;
  if (override) {
    return { address: `ws://127.0.0.1:${override}`, source: "RCDVLOG_EVAL_BRIDGE_PORT" };
  }

  const path = addressFilePath(env);
  const deadline = Date.now() + waitMs;
  let published = readPublishedAddress(path);
  while (published.state !== "running" && Date.now() < deadline) {
    await delay(100);
    published = readPublishedAddress(path);
  }

  if (published.state === "running") return { address: published.address, source: path };
  throw new Error(describeMissingBridge(published));
}

function describeMissingBridge({ state, path, pid }) {
  const start = "Start the desktop app with RCDVLOG_EVAL_BRIDGE=1, or pass --port.";
  switch (state) {
    case "stale":
      return `The eval bridge address at ${path} belongs to process ${pid}, which is gone. ${start}`;
    case "unreadable":
      return `Could not read an eval bridge address from ${path}. ${start}`;
    default:
      return `No eval bridge address at ${path}. ${start}`;
  }
}
