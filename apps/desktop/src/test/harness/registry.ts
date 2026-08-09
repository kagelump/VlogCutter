/**
 * The command table.
 *
 * Every supported command is registered by name. There is no permissive
 * default: an unregistered command rejects, naming itself, so adding a new
 * production `invoke()` fails the CUJ suite until the harness learns it. A
 * `return null` fallback would let the fake rot silently while the tests stayed
 * green, which is the failure mode this whole harness exists to avoid.
 */

import type { HarnessClock } from "./clock";
import type { EventBus } from "./events";
import type { JobRegistry } from "./jobs";
import type { DesktopHarnessState } from "./state";

export interface HarnessContext {
  state: DesktopHarnessState;
  clock: HarnessClock;
  events: EventBus;
  jobs: JobRegistry;
  emit(event: string, payload: unknown): number;
}

export type CommandArgs = Record<string, unknown>;
export type CommandHandler = (args: CommandArgs, context: HarnessContext) => unknown;
export type CommandTable = Record<string, CommandHandler>;

export function mergeTables(...tables: CommandTable[]): CommandTable {
  const merged: CommandTable = {};
  for (const table of tables) {
    for (const [command, handler] of Object.entries(table)) {
      if (merged[command]) {
        throw new Error(`Duplicate harness handler for command: ${command}`);
      }
      merged[command] = handler;
    }
  }
  return merged;
}

/** Arguments cross the bridge in camelCase. A handler that guesses hides an adapter bug. */
export function requireString(args: CommandArgs, key: string): string {
  const value = args[key];
  if (typeof value !== "string") {
    throw new Error(
      `Harness command argument "${key}" must be a string, received ${JSON.stringify(value)}. ` +
        `Arguments seen: ${Object.keys(args).join(", ") || "(none)"}`,
    );
  }
  return value;
}

export function requireBoolean(args: CommandArgs, key: string): boolean {
  const value = args[key];
  if (typeof value !== "boolean") {
    throw new Error(`Harness command argument "${key}" must be a boolean, received ${JSON.stringify(value)}`);
  }
  return value;
}

export function requireNumber(args: CommandArgs, key: string): number {
  const value = args[key];
  if (typeof value !== "number") {
    throw new Error(`Harness command argument "${key}" must be a number, received ${JSON.stringify(value)}`);
  }
  return value;
}
