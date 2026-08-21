/**
 * The harness: one canonical state, one command table, one event transport, and
 * the controls a test drives them with.
 *
 * Everything above this file is production code. The application, its API
 * adapters, the Tauri client library, the command names and the DTO shapes are
 * all real; this replaces only where the call lands.
 */

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
import { mediaCommands } from "./commands/media";
import { editPlanCommands } from "./commands/editPlan";
import { MediaController } from "./media";
import type { ArchiveRoot, Asset, CardCandidate } from "../../types";

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
  mediaCommands,
  editPlanCommands,
);

/** Commands served by the event plugin rather than by a handler module. */
export const EVENT_COMMANDS = ["plugin:event|listen", "plugin:event|unlisten"] as const;

/**
 * How a harness-initiated React update is wrapped.
 *
 * Under Vitest this is `act` from `@testing-library/react`, supplied by
 * `renderApp`. It is injected rather than imported here because the browser
 * preview installs this same harness (`src/preview/`), and a static import of a
 * devDependency would drag the testing library into the shipped bundle. The
 * default is a passthrough: outside a test there is nothing to flush.
 */
export type ActWrapper = <T>(action: () => Promise<T> | T) => Promise<T>;

const passthrough: ActWrapper = async (action) => action();

export interface HarnessOptions {
  /** Wrapper for updates the harness pushes into React. Defaults to a passthrough. */
  act?: ActWrapper;
  /**
   * Thumbnail URL builder. The default asset-protocol stub is a URL nothing can
   * fetch, which is all a CUJ needs; the browser preview overrides it so the
   * chronology canvas has frames to draw.
   */
  convertFileSrc?: (filePath: string, protocol?: string) => string;
}

export class DesktopHarness {
  readonly clock: HarnessClock;
  readonly state: DesktopHarnessState;
  readonly events = new EventBus();
  readonly faults = new FaultRegistry();
  readonly jobs: JobRegistry;

  private log: CommandLogEntry[] = [];
  private uninstallRuntime: (() => void) | null = null;
  private actWrapper: ActWrapper;
  private convertFileSrc: HarnessOptions["convertFileSrc"];

  constructor(state: DesktopHarnessState, clock: HarnessClock, options: HarnessOptions = {}) {
    this.state = state;
    this.clock = clock;
    this.actWrapper = options.act ?? passthrough;
    this.convertFileSrc = options.convertFileSrc;
    this.jobs = new JobRegistry(
      (event, payload) => this.events.emit(event, payload),
      (action) => this.inAct(action),
    );
  }

  // ── lifecycle ──────────────────────────────────────────────────────

  /**
   * Wrap harness-initiated updates. `renderApp` calls this with `act` before it
   * mounts, which is the only place the testing library is reached from.
   */
  useActWrapper(wrapper: ActWrapper): this {
    this.actWrapper = wrapper;
    return this;
  }

  install(): this {
    this.uninstallRuntime = installTauriRuntime({
      invoke: (command, args) => this.invoke(command, args ?? {}),
      transformCallback: (callback, once) => this.events.transformCallback(callback, once),
      unregisterListener: (event, eventId) => this.events.unlisten(event, eventId),
      convertFileSrc: this.convertFileSrc,
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

    const fault = this.faults.take(command, args);
    if (fault?.kind === "reject") {
      entry.outcome = "rejected";
      entry.error = fault.message ?? command;
      throw new Error(fault.message ?? `${command} failed`);
    }
    if (fault?.kind === "delay") {
      await this.faults.gate(command);
      // A held call that fails when it is finally answered: the late rejection
      // is a different thing from a late success, and both have to be shown
      // not to reach a session that has moved on.
      if (fault.message) {
        entry.outcome = "rejected";
        entry.error = fault.message;
        throw new Error(fault.message);
      }
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
   * here rather than in every test. The wrapper itself is injected; see
   * `ActWrapper`.
   */
  private inAct<T>(action: () => Promise<T> | T): Promise<T> {
    return this.actWrapper(action);
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
  next(command: string, match?: (args: CommandArgs) => boolean) {
    return {
      reject: (message: string) => {
        this.faults.queue(command, { kind: "reject", message, match });
        return this;
      },
      delayUntilReleased: () => {
        this.faults.queue(command, { kind: "delay", match });
        return this;
      },
      /** Hold the call, then fail it when released. */
      delayThenReject: (message: string) => {
        this.faults.queue(command, { kind: "delay", message, match });
        return this;
      },
    };
  }

  /**
   * Fail or delay every call to a command until cleared.
   *
   * The optional matcher narrows the fault to the calls whose arguments satisfy
   * it, which is how one project's read is held while another's is answered:
   *
   *     harness.command("list_assets", (args) => args.projectId === "project_a")
   *       .delayUntilReleased();
   */
  command(command: string, match?: (args: CommandArgs) => boolean) {
    return {
      reject: (message: string) => {
        this.faults.always(command, { kind: "reject", message, match });
        return this;
      },
      delayUntilReleased: () => {
        this.faults.always(command, { kind: "delay", match });
        return this;
      },
      /** Hold every matching call, then fail them when released. */
      delayThenReject: (message: string) => {
        this.faults.always(command, { kind: "delay", message, match });
        return this;
      },
      clear: () => {
        this.faults.clearCommand(command);
        return this;
      },
    };
  }

  /** Reads better than `command(...)` when the fault is meant to persist. */
  always(command: string, match?: (args: CommandArgs) => boolean) {
    return this.command(command, match);
  }

  /** Let a delayed command through. */
  release(command: string) {
    this.faults.releaseGate(command);
  }

  clearFaults() {
    this.faults.clear();
  }

  /**
   * Drive the mounted media element for one clip. Real DOM events at the real
   * element; never evidence that anything decoded.
   */
  media(assetId: string): MediaController {
    return new MediaController(assetId, (action) => this.inAct(action));
  }

  /**
   * The removable volume that goes away and comes back (#67).
   *
   * `eject` is an unmount: a brownout, a nudged cable, a card pulled a second
   * early. `remount` puts the *same* card back — same session identity, which
   * is the only thing resume is allowed to match on — optionally at a
   * different mount path, because that is what actually happens when a volume
   * reappears while its old name is still taken.
   */
  card(mountPath: string) {
    const harness = this;
    return {
      eject(): CardCandidate {
        const index = harness.state.cards.findIndex((card) => card.mountPath === mountPath);
        if (index < 0) throw new Error(`No card is mounted at ${mountPath}`);
        return harness.state.cards.splice(index, 1)[0];
      },
      remount(card: CardCandidate, at: string = mountPath): CardCandidate {
        const moved = { ...card, mountPath: at };
        if (at !== card.mountPath) {
          // Everything the harness keys by mount path travels with the volume.
          // The session identity is the part that does not move.
          harness.state.imports.additions[at] = harness.state.imports.additions[card.mountPath] ?? [];
          delete harness.state.imports.additions[card.mountPath];
          harness.state.imports.cameraExifHints[at] = harness.state.imports.cameraExifHints[card.mountPath] ?? "";
          delete harness.state.imports.cameraExifHints[card.mountPath];
        }
        harness.state.cards.push(moved);
        return moved;
      },
      /** A *different* card in the same slot. Same path, new identity. */
      swapForDifferentCard(sessionId: string): CardCandidate {
        const index = harness.state.cards.findIndex((card) => card.mountPath === mountPath);
        const previous = index < 0 ? null : harness.state.cards.splice(index, 1)[0];
        const intruder: CardCandidate = {
          sessionId,
          label: previous?.label ?? "Untitled",
          mountPath,
          cameraHint: previous?.cameraHint ?? "",
          clipCount: 1,
          totalBytes: 1_000_000,
        };
        harness.state.cards.push(intruder);
        return intruder;
      },
    };
  }

  /**
   * The archive destination that goes away and comes back (#79).
   *
   * The mirror of `card`, for the other end of the copy. `disconnect` is the
   * enclosure dropping off the bus mid-write; `reconnect` brings back the same
   * volume, and `swapForDifferentVolume` stages the case #78 exists to catch —
   * a drive answering at the remembered path that is not the drive the import
   * was writing to.
   */
  archive(archiveRootId: string) {
    const harness = this;
    const find = () => {
      const root = harness.state.archiveRoots.find((entry) => entry.id === archiveRootId);
      if (!root) throw new Error(`No archive root ${archiveRootId}`);
      return root;
    };
    const update = (patch: Partial<ArchiveRoot>) => {
      harness.state.archiveRoots = harness.state.archiveRoots.map((entry) =>
        entry.id === archiveRootId ? { ...entry, ...patch } : entry,
      );
    };
    return {
      disconnect() {
        find();
        update({ available: false });
      },
      reconnect() {
        const root = find();
        if (root.localMountPath) delete harness.state.mountedVolumeIds[root.localMountPath];
        update({ available: true });
      },
      /** Same path, different hardware. Identity must refuse this one. */
      swapForDifferentVolume(volumeId: string) {
        const root = find();
        if (root.localMountPath) harness.state.mountedVolumeIds[root.localMountPath] = volumeId;
        update({ available: true });
      },
    };
  }

  /** Direct state edits that read like the condition they represent. */
  asset(assetId: string) {
    const harness = this;
    return {
      get current(): Asset {
        return requireAsset(harness.state, assetId);
      },
      setOriginalAvailable(available: boolean) {
        requireAsset(harness.state, assetId);
        patchAsset(harness.state, assetId, { originalAvailable: available });
        return this;
      },
      setProxyAvailable(available: boolean) {
        const asset = requireAsset(harness.state, assetId);
        patchAsset(harness.state, assetId, {
          proxyAvailable: available,
          proxyPath: available ? asset.proxyPath ?? `/Users/harness/Movies/proxies/${assetId}.v3.mp4` : asset.proxyPath,
          proxyRepresentation: available ? asset.proxyRepresentation ?? "raw_v3" : asset.proxyRepresentation,
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
