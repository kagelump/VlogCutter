/**
 * Fault injection.
 *
 * The strongest reason this harness exists. A failed note save, a volume that
 * goes offline mid-review, a sync that will not connect, and a command that
 * never returns are all expensive or impossible to stage against real hardware,
 * and all cheap here.
 *
 * Faults are one-shot by default (`next`), so the retry after a failure runs the
 * real success path through the same production call.
 */

import type { CommandArgs } from "./registry";

export interface Fault {
  kind: "reject" | "delay";
  message?: string;
  /**
   * Which calls this fault applies to. A command is usually named once per
   * test, but a session race needs the same command treated differently
   * depending on its arguments — project A's `list_assets` held while project
   * B's is answered.
   */
  match?(args: CommandArgs): boolean;
}

class Deferred<T> {
  readonly promise: Promise<T>;
  resolve!: (value: T) => void;
  reject!: (reason: unknown) => void;

  constructor() {
    this.promise = new Promise<T>((resolve, reject) => {
      this.resolve = resolve;
      this.reject = reject;
    });
  }
}

export class FaultRegistry {
  private oneShot = new Map<string, Fault[]>();
  private persistent = new Map<string, Fault[]>();
  /** Every call currently held at a gate, so releasing frees all of them. */
  private gates = new Map<string, Deferred<void>[]>();

  queue(command: string, fault: Fault) {
    const queued = this.oneShot.get(command) ?? [];
    queued.push(fault);
    this.oneShot.set(command, queued);
  }

  always(command: string, fault: Fault) {
    const existing = this.persistent.get(command) ?? [];
    // A narrower fault is added rather than replacing what is there: two
    // argument-matched faults on one command is the whole point of matching.
    this.persistent.set(command, fault.match ? [...existing, fault] : [fault]);
  }

  clearCommand(command: string) {
    this.oneShot.delete(command);
    this.persistent.delete(command);
    this.releaseGate(command);
  }

  clear() {
    this.oneShot.clear();
    this.persistent.clear();
    for (const command of [...this.gates.keys()]) this.releaseGate(command);
  }

  /**
   * Consume the fault that applies to this call, if any. One-shot faults are
   * taken in the order they were queued; a persistent fault applies to every
   * matching call until cleared. A fault with a matcher is skipped for calls it
   * does not match, so an unmatched call runs the real handler.
   */
  take(command: string, args: CommandArgs = {}): Fault | null {
    const queued = this.oneShot.get(command);
    if (queued?.length) {
      const index = queued.findIndex((fault) => !fault.match || fault.match(args));
      if (index >= 0) {
        const [fault] = queued.splice(index, 1);
        if (!queued.length) this.oneShot.delete(command);
        return fault;
      }
    }
    return (this.persistent.get(command) ?? []).find((fault) => !fault.match || fault.match(args)) ?? null;
  }

  /**
   * The promise a delayed call waits on. One per held call rather than one per
   * command, so two calls to the same command can be held at once and released
   * together.
   */
  gate(command: string): Promise<void> {
    const deferred = new Deferred<void>();
    this.gates.set(command, [...(this.gates.get(command) ?? []), deferred]);
    return deferred.promise;
  }

  releaseGate(command: string) {
    const held = this.gates.get(command);
    if (!held) return;
    this.gates.delete(command);
    held.forEach((deferred) => deferred.resolve());
  }
}
