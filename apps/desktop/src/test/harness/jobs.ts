/**
 * Scripted background work.
 *
 * A long operation — a proxy batch, an analysis run, an import — does not
 * resolve on its own here. The command promise stays pending until the test
 * completes, cancels, or fails it, and each progress step is delivered when the
 * test says so. That is what makes "cancel arrives between the last progress
 * event and completion" a test rather than a hope, and it is why no CUJ needs to
 * wait on a real timer.
 */

export class Deferred<T> {
  readonly promise: Promise<T>;
  resolve!: (value: T) => void;
  reject!: (reason: unknown) => void;
  settled = false;

  constructor() {
    this.promise = new Promise<T>((resolve, reject) => {
      this.resolve = (value: T) => {
        this.settled = true;
        resolve(value);
      };
      this.reject = (reason: unknown) => {
        this.settled = true;
        reject(reason);
      };
    });
    // Nobody may see an unhandled rejection for a job the test failed on
    // purpose; the command's own caller is the one that must observe it.
    this.promise.catch(() => {});
  }
}

/**
 * What a command family contributes when it defers to the test: the event its
 * progress travels on, how a test-supplied step becomes that event's payload,
 * and how an outcome becomes the command's return value.
 */
export interface JobBinding<TStep = unknown, TOutcome = unknown, TResult = unknown> {
  command: string;
  args: Record<string, unknown>;
  event: string | null;
  progress(step: TStep): unknown;
  result(outcome: TOutcome): TResult;
  /** Called when the test cancels, before `result` builds the return value. */
  onCancel?(): void;
}

export class JobController<TStep = any, TOutcome = any> {
  private binding: JobBinding<TStep, TOutcome> | null = null;
  private bound = new Deferred<void>();
  private settlement = new Deferred<unknown>();
  private cancelled = false;

  constructor(
    readonly command: string,
    private readonly emit: (event: string, payload: unknown) => number,
    private readonly wrap: <T>(action: () => Promise<T> | T) => Promise<T>,
  ) {}

  /** Handler side: the command has been invoked and is now the test's to drive. */
  bind(binding: JobBinding<TStep, TOutcome>): Promise<unknown> {
    this.binding = binding;
    this.bound.resolve();
    return this.settlement.promise;
  }

  /** Resolves once the application has actually issued the command. */
  async started(): Promise<void> {
    await this.bound.promise;
  }

  /** Deliver one progress step through the real event transport. */
  async progress(step: TStep): Promise<void> {
    await this.started();
    const binding = this.binding!;
    await this.wrap(() => {
      const payload = binding.progress(step);
      if (binding.event) this.emit(binding.event, payload);
    });
  }

  /** Resolve the pending command with a successful outcome. */
  async complete(outcome?: TOutcome): Promise<void> {
    await this.started();
    const binding = this.binding!;
    await this.wrap(() => {
      this.settlement.resolve(binding.result((outcome ?? {}) as TOutcome));
    });
  }

  /**
   * Record that cancellation was asked for, without settling the command. This
   * is what the production `cancel_*` command does: it asks the run to stop,
   * and the run finishes on its own terms afterwards.
   */
  requestCancel() {
    this.cancelled = true;
  }

  /** Mark the run cancelled and resolve it with whatever it had reached. */
  async cancel(outcome?: TOutcome): Promise<void> {
    await this.started();
    this.cancelled = true;
    const binding = this.binding!;
    await this.wrap(() => {
      binding.onCancel?.();
      this.settlement.resolve(binding.result((outcome ?? {}) as TOutcome));
    });
  }

  /** Reject the pending command, as a backend error would. */
  async fail(message: string): Promise<void> {
    await this.started();
    await this.wrap(() => {
      this.settlement.reject(new Error(message));
    });
  }

  get isCancelled() {
    return this.cancelled;
  }

  get isSettled() {
    return this.settlement.settled;
  }
}

export class JobRegistry {
  private controllers = new Map<string, JobController>();

  constructor(
    private readonly emit: (event: string, payload: unknown) => number,
    private readonly wrap: <T>(action: () => Promise<T> | T) => Promise<T>,
  ) {}

  /** Test side: claim a command so its next invocation waits for this controller. */
  claim<TStep = unknown, TOutcome = unknown>(command: string): JobController<TStep, TOutcome> {
    const existing = this.controllers.get(command);
    if (existing && !existing.isSettled) return existing as JobController<TStep, TOutcome>;
    const controller = new JobController<TStep, TOutcome>(command, this.emit, this.wrap);
    this.controllers.set(command, controller);
    return controller;
  }

  /** Handler side: is this call scripted? */
  claimed(command: string): JobController | null {
    const controller = this.controllers.get(command);
    if (!controller || controller.isSettled) return null;
    return controller;
  }

  reset() {
    this.controllers.clear();
  }
}
