/**
 * The harness: one canonical state, one command table, one event transport, and
 * the controls a test drives them with.
 *
 * Everything above this file is production code. The application, its API
 * adapters, the Tauri client library, the command names and the DTO shapes are
 * all real; this replaces only where the call lands.
 */

import { act } from "@testing-library/react";
import { HarnessClock } from "./clock";
import { EventBus } from "./events";
import { FaultRegistry } from "./faults";
import { JobRegistry, type JobController } from "./jobs";
import { installTauriRuntime } from "./ipc";
import { mergeTables, type CommandArgs, type CommandTable, type HarnessContext } from "./registry";
import { patchAsset, requireAsset, type DesktopHarnessState } from "./state";
import { coreCommands } from "./commands/core";
import { reviewCommands } from "./commands/review";
import { analysisCommands } from "./commands/analysis";
import { chronologyCommands } from "./commands/chronology";
import { serverCommands } from "./commands/server";
import { importCommands } from "./commands/import";
import type { Asset } from "../../types";

export interface CommandLogEntry {
  command: string;
  args: CommandArgs;
  outcome: "resolved" | "rejected" | "unhandled";
  error?: string;
}

export const commandTable: CommandTable = mergeTables(
  coreCommands,
  reviewCommands,
  analysisCommands,
  chronologyCommands,
  serverCommands,
  importCommands,
);

/** Commands served by the event plugin rather than by a handler module. */
export const EVENT_COMMANDS = ["plugin:event|listen", "plugin:event|unlisten"] as const;

export class DesktopHarness {
  readonly clock: HarnessClock;
  readonly state: DesktopHarnessState;
  readonly events = new EventBus();
  readonly faults = new FaultRegistry();
  readonly jobs: JobRegistry;

  private log: CommandLogEntry[] = [];
  private uninstallRuntime: (() => void) | null = null;

  constructor(state: DesktopHarnessState, clock: HarnessClock) {
    this.state = state;
    this.clock = clock;
    this.jobs = new JobRegistry(
      (event, payload) => this.events.emit(event, payload),
      (action) => this.inAct(action),
    );
  }

  // ── lifecycle ──────────────────────────────────────────────────────

  install(): this {
    this.uninstallRuntime = installTauriRuntime({
      invoke: (command, args) => this.invoke(command, args ?? {}),
      transformCallback: (callback, once) => this.events.transformCallback(callback, once),
      unregisterListener: (event, eventId) => this.events.unlisten(event, eventId),
    });
    return this;
  }

  uninstall() {
    this.uninstallRuntime?.();
    this.uninstallRuntime = null;
    this.events.reset();
    this.faults.clear();
    this.jobs.reset();
  }

  get installed() {
    return this.uninstallRuntime !== null;
  }

  // ── dispatch ───────────────────────────────────────────────────────

  private context(): HarnessContext {
    return {
      state: this.state,
      clock: this.clock,
      events: this.events,
      jobs: this.jobs,
      emit: (event, payload) => this.events.emit(event, payload),
    };
  }

  private async invoke(command: string, args: CommandArgs): Promise<unknown> {
    const entry: CommandLogEntry = { command, args, outcome: "resolved" };
    this.log.push(entry);

    const fault = this.faults.take(command);
    if (fault?.kind === "reject") {
      entry.outcome = "rejected";
      entry.error = fault.message ?? command;
      throw new Error(fault.message ?? `${command} failed`);
    }
    if (fault?.kind === "delay") {
      await this.faults.gate(command);
    }

    if (command === "plugin:event|listen") {
      const event = String(args.event);
      const handler = Number(args.handler);
      return this.events.listen(event, handler);
    }
    if (command === "plugin:event|unlisten") {
      this.events.unlisten(String(args.event), Number(args.eventId));
      return null;
    }

    const handler = commandTable[command];
    if (!handler) {
      entry.outcome = "unhandled";
      const message =
        `Unhandled Tauri command in test harness: ${command}. ` +
        "Add a handler in src/test/harness/commands/, or assert the rejection. " +
        "The harness has no permissive default on purpose: a new production invoke() " +
        "must fail here rather than quietly return null.";
      entry.error = message;
      throw new Error(message);
    }

    try {
      return await handler(args, this.context());
    } catch (reason) {
      entry.outcome = "rejected";
      entry.error = reason instanceof Error ? reason.message : String(reason);
      throw reason;
    }
  }

  // ── observation ────────────────────────────────────────────────────

  /** Every command the application issued, in order. */
  commandLog(): CommandLogEntry[] {
    return [...this.log];
  }

  /** The argument objects a command was called with. */
  calls(command: string): CommandArgs[] {
    return this.log.filter((entry) => entry.command === command).map((entry) => entry.args);
  }

  /** The command names in issue order, for ordering contracts such as preview-before-write. */
  commandNames(): string[] {
    return this.log.map((entry) => entry.command);
  }

  clearLog() {
    this.log = [];
  }

  // ── driving ────────────────────────────────────────────────────────

  /**
   * Anything the harness pushes into the application — an event, a job step —
   * is a React update originating outside a user interaction, so it is wrapped
   * here rather than in every test.
   */
  private async inAct<T>(action: () => Promise<T> | T): Promise<T> {
    let result!: T;
    await act(async () => {
      result = await action();
    });
    return result;
  }

  /** Emit a Tauri event to the application's live listeners. Returns how many were reached. */
  async emit(event: string, payload: unknown): Promise<number> {
    return this.inAct(() => this.events.emit(event, payload));
  }

  /** Claim a long-running command so the test decides its progress and its ending. */
  job<TStep = unknown, TOutcome = unknown>(command: string): JobController<TStep, TOutcome> {
    return this.jobs.claim<TStep, TOutcome>(command);
  }

  /** Fail the next call to a command. One-shot, so the retry runs the success path. */
  next(command: string) {
    return {
      reject: (message: string) => {
        this.faults.queue(command, { kind: "reject", message });
        return this;
      },
      delayUntilReleased: () => {
        this.faults.queue(command, { kind: "delay" });
        return this;
      },
    };
  }

  /** Fail or delay every call to a command until cleared. */
  command(command: string) {
    return {
      reject: (message: string) => {
        this.faults.always(command, { kind: "reject", message });
        return this;
      },
      delayUntilReleased: () => {
        this.faults.always(command, { kind: "delay" });
        return this;
      },
      clear: () => {
        this.faults.clearCommand(command);
        return this;
      },
    };
  }

  /** Let a delayed command through. */
  release(command: string) {
    this.faults.releaseGate(command);
  }

  clearFaults() {
    this.faults.clear();
  }

  /** Direct state edits that read like the condition they represent. */
  asset(assetId: string) {
    const harness = this;
    return {
      get current(): Asset {
        return requireAsset(harness.state, assetId);
      },
      setOriginalAvailable(available: boolean) {
        const asset = requireAsset(harness.state, assetId);
        patchAsset(harness.state, assetId, {
          originalAvailable: available,
          available: available || asset.proxyAvailable,
        });
        return this;
      },
      setProxyAvailable(available: boolean) {
        const asset = requireAsset(harness.state, assetId);
        patchAsset(harness.state, assetId, {
          proxyAvailable: available,
          proxyPath: available ? asset.proxyPath ?? `/Users/harness/Movies/proxies/${assetId}.mp4` : asset.proxyPath,
          available: available || asset.originalAvailable,
        });
        return this;
      },
      patch(patch: Partial<Asset>) {
        patchAsset(harness.state, assetId, patch);
        return this;
      },
    };
  }
}
