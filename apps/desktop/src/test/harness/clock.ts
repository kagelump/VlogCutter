/**
 * The harness clock.
 *
 * Every timestamp a fake command produces comes from here, so two runs of the
 * same scenario produce byte-identical fixtures and a failure is readable. This
 * is deliberately *not* Vitest's fake timers: a CUJ that does not care about
 * `App`'s three real timers should not pay for intercepting them, and a CUJ that
 * does care opts into `vi.useFakeTimers()` separately.
 */
export class HarnessClock {
  private currentMs: number;

  constructor(startIso = "2026-08-09T12:00:00.000Z") {
    this.currentMs = Date.parse(startIso);
  }

  /** Move the clock to an absolute instant. */
  set(iso: string) {
    const parsed = Date.parse(iso);
    if (Number.isNaN(parsed)) throw new Error(`Harness clock cannot parse: ${iso}`);
    this.currentMs = parsed;
  }

  /** Move the clock forward. Negative values are rejected: time is a fixture, not a toy. */
  advance(milliseconds: number) {
    if (milliseconds < 0) throw new Error("The harness clock does not run backwards");
    this.currentMs += milliseconds;
  }

  now(): number {
    return this.currentMs;
  }

  /** The ISO form every DTO field uses. */
  iso(): string {
    return new Date(this.currentMs).toISOString();
  }

  /** An instant relative to now, without moving the clock. */
  isoAfter(milliseconds: number): string {
    return new Date(this.currentMs + milliseconds).toISOString();
  }
}
