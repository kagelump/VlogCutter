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

/**
 * Commands Rust registers that no frontend call reaches, each with the decision
 * that left it that way. This is an inventory rather than a failure: the CLI and
 * the Rust tests are legitimate callers. What it prevents is a command drifting
 * out of the interface unnoticed — the state the narrow asset writers were in
 * for as long as `persistAsset` sent the whole patch while seven adapters sat
 * beside it implying otherwise (#19).
 */
export const uncalledBackendCommands: Array<{ command: string; reason: string }> = [
  {
    command: "add_label",
    reason:
      "Decided in #19: the desktop writes a judgment as one whole AssetPatch through update_asset, and the mutation lifecycle owns that single path. The adapter was removed; the command stays for the CLI.",
  },
  {
    command: "chronology_solution",
    reason:
      "The solved-order dump is a CLI diagnostic. The desktop reads chronology_overview, which carries what the ribbon and the calibration panel render.",
  },
  {
    command: "create_note_revision",
    reason:
      "Decided in #19: notes are written as part of the whole patch, so the desktop does not create note revisions. Removing the adapter stops the frontend implying a revision history it never wrote. Routing notes through this command is a product decision, not a refactor.",
  },
  {
    command: "generate_thumbnail",
    reason:
      "Only the batch is reachable in the interface (generate_missing_thumbnails); a single-clip thumbnail has no affordance, so the adapter was removed rather than left as a dead export.",
  },
  {
    command: "ingest_folder",
    reason:
      "Superseded by the smart-import flow: the desktop classifies a source with preflight_import and writes with run_smart_import. The bare recursive ingest remains a CLI entry point.",
  },
  {
    command: "remove_label",
    reason: "Same decision as add_label: one whole-patch write path through update_asset.",
  },
  {
    command: "set_removed",
    reason: "Same decision as add_label: removal rides in the AssetPatch the lifecycle sends.",
  },
  {
    command: "set_rotation",
    reason:
      "Same decision as add_label: rotation is part of the asset contract and is sent with the patch, which review.cuj.test.tsx asserts directly.",
  },
];
