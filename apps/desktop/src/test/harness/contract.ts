/**
 * The three command surfaces, extracted rather than listed.
 *
 * A hand-maintained list of commands would drift the moment someone added an
 * `invoke()`, and drift is exactly what this is for. So each set is read from
 * the source that defines it:
 *
 *   frontend — every `invoke("…")` in the API adapters
 *   harness  — the keys of the handler table
 *   backend  — the `tauri::generate_handler![…]` registration
 *
 * The extractor is deliberately literal: it recognises `invoke("name")` and
 * nothing else. A templated or computed command name is reported by
 * `dynamicInvocations()` so the suite can fail closed on it rather than
 * silently miss it.
 */

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { commandTable, EVENT_COMMANDS } from "./DesktopHarness";

const here = dirname(fileURLToPath(import.meta.url));
const desktopRoot = resolve(here, "../../..");

export const ADAPTER_SOURCES = ["src/api.ts", "src/chronologyApi.ts"] as const;
export const BACKEND_SOURCE = "src-tauri/src/lib.rs";

function read(relativePath: string): string {
  return readFileSync(resolve(desktopRoot, relativePath), "utf8");
}

/** Every literal `invoke("name")` the frontend can issue. */
export function frontendCommands(): Set<string> {
  const found = new Set<string>();
  for (const source of ADAPTER_SOURCES) {
    for (const match of read(source).matchAll(/\binvoke\s*(?:<[^>]*>)?\s*\(\s*"([^"]+)"/g)) {
      found.add(match[1]);
    }
  }
  return found;
}

/**
 * Invocations whose command name is not a literal string. Any of these is
 * invisible to the extractor above, so the suite treats a non-empty result as a
 * failure rather than as coverage.
 */
export function dynamicInvocations(): string[] {
  const found: string[] = [];
  for (const source of ADAPTER_SOURCES) {
    read(source)
      .split("\n")
      .forEach((line, index) => {
        if (!/\binvoke\s*(?:<[^>]*>)?\s*\(/.test(line)) return;
        if (/\binvoke\s*(?:<[^>]*>)?\s*\(\s*"/.test(line)) return;
        found.push(`${source}:${index + 1}: ${line.trim()}`);
      });
  }
  return found;
}

/** Every command registered on the Rust side. */
export function backendCommands(): Set<string> {
  const source = read(BACKEND_SOURCE);
  const start = source.indexOf("tauri::generate_handler![");
  if (start < 0) throw new Error(`No generate_handler! block found in ${BACKEND_SOURCE}`);
  const end = source.indexOf("])", start);
  const block = source.slice(start, end);
  const found = new Set<string>();
  for (const line of block.split("\n").slice(1)) {
    const name = line.trim().replace(/,$/, "");
    if (!name || name.startsWith("//")) continue;
    // `module::command` registers under the bare command name.
    found.add(name.includes("::") ? name.slice(name.lastIndexOf("::") + 2) : name);
  }
  return found;
}

export function harnessCommands(): Set<string> {
  return new Set([...Object.keys(commandTable), ...EVENT_COMMANDS]);
}

/**
 * Commands the harness deliberately does not implement, each with the reason.
 * An entry with an empty reason fails the suite: "we did not get to it" is a
 * statement worth writing down, not a blank.
 */
export const contractExemptions: Array<{ command: string; reason: string }> = [
  {
    command: "plugin:dialog|open",
    reason:
      "Served by the harness as a scriptable command, not by a handler module: native dialogs are a Tauri boundary and this only stands in for what the user picked.",
  },
];

export function exemptCommands(): Set<string> {
  return new Set(contractExemptions.map((entry) => entry.command));
}
