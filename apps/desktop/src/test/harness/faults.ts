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

export interface Fault {
  kind: "reject" | "delay";
  message?: string;
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
  private persistent = new Map<string, Fault>();
  private gates = new Map<string, Deferred<void>>();

  queue(command: string, fault: Fault) {
    const queued = this.oneShot.get(command) ?? [];
    queued.push(fault);
    this.oneShot.set(command, queued);
  }

  always(command: string, fault: Fault) {
    this.persistent.set(command, fault);
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
   * call until cleared.
   */
  take(command: string): Fault | null {
    const queued = this.oneShot.get(command);
    if (queued?.length) {
      const fault = queued.shift()!;
      if (!queued.length) this.oneShot.delete(command);
      return fault;
    }
    return this.persistent.get(command) ?? null;
  }

  /** The promise a delayed command waits on. Created lazily so `release` before `invoke` is legal. */
  gate(command: string): Promise<void> {
    let deferred = this.gates.get(command);
    if (!deferred) {
      deferred = new Deferred<void>();
      this.gates.set(command, deferred);
    }
    return deferred.promise;
  }

  releaseGate(command: string) {
    const deferred = this.gates.get(command);
    if (!deferred) return;
    this.gates.delete(command);
    deferred.resolve();
  }
}
