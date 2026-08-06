import { describe, expect, it } from "vitest";
import {
  DEFAULT_PANEL_SIZES,
  PANEL_LIMITS,
  PANEL_SIZES_KEY,
  clampPanel,
  loadPanelSizes,
  savePanelSizes,
} from "./panelSizes";

function storage(initial: Record<string, string> = {}) {
  const store = new Map(Object.entries(initial));
  return {
    store,
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => void store.set(key, value),
  };
}

describe("clampPanel", () => {
  it("holds a value inside the panel's own limits", () => {
    expect(clampPanel("queue", 340, 1440)).toBe(340);
    expect(clampPanel("queue", 10, 1440)).toBe(PANEL_LIMITS.queue.min);
    expect(clampPanel("queue", 9000, 1440)).toBe(PANEL_LIMITS.queue.max);
  });

  it("never lets a drag take more than half the viewport", () => {
    // 0.5 * 800 = 400, below the queue's 520 ceiling.
    expect(clampPanel("queue", 500, 800)).toBe(400);
  });

  it("keeps the minimum even when half the viewport is smaller than it", () => {
    expect(clampPanel("rail", 100, 400)).toBe(PANEL_LIMITS.rail.min);
    expect(clampPanel("rail", 900, 400)).toBe(PANEL_LIMITS.rail.min);
  });

  it("rounds to whole pixels, because pointer deltas are fractional", () => {
    expect(clampPanel("rail", 401.6, 1440)).toBe(402);
  });

  it("falls back to the minimum for a non-finite value", () => {
    expect(clampPanel("ribbon", Number.NaN, 900)).toBe(PANEL_LIMITS.ribbon.min);
  });

  it("lets the ribbon grow well past a single lane, which is the point", () => {
    expect(clampPanel("ribbon", 320, 919)).toBe(320);
  });
});

describe("loadPanelSizes", () => {
  it("returns the defaults when nothing is stored", () => {
    expect(loadPanelSizes(storage())).toEqual(DEFAULT_PANEL_SIZES);
  });

  it("returns the defaults when there is no storage at all", () => {
    expect(loadPanelSizes(null)).toEqual(DEFAULT_PANEL_SIZES);
  });

  it("reads back what was saved", () => {
    const store = storage();
    savePanelSizes(store, { queue: 260, rail: 520, ribbon: 300 });
    expect(loadPanelSizes(store)).toEqual({ queue: 260, rail: 520, ribbon: 300 });
  });

  it("treats a null ribbon as size-to-content", () => {
    const store = storage({ [PANEL_SIZES_KEY]: JSON.stringify({ queue: 300, rail: 400, ribbon: null }) });
    expect(loadPanelSizes(store).ribbon).toBeNull();
  });

  it("falls back to the defaults rather than throwing on garbage", () => {
    expect(loadPanelSizes(storage({ [PANEL_SIZES_KEY]: "{not json" }))).toEqual(DEFAULT_PANEL_SIZES);
  });

  it("fills in individually missing fields", () => {
    const store = storage({ [PANEL_SIZES_KEY]: JSON.stringify({ queue: 280 }) });
    expect(loadPanelSizes(store)).toEqual({ queue: 280, rail: 400, ribbon: null });
  });

  it("survives a storage that throws on read", () => {
    const hostile = {
      getItem() { throw new Error("denied"); },
      setItem() {},
    };
    expect(loadPanelSizes(hostile)).toEqual(DEFAULT_PANEL_SIZES);
  });
});

describe("savePanelSizes", () => {
  it("writes under a versioned key", () => {
    const store = storage();
    savePanelSizes(store, DEFAULT_PANEL_SIZES);
    expect(store.store.has(PANEL_SIZES_KEY)).toBe(true);
  });

  it("does not throw when the store refuses to write", () => {
    const hostile = {
      getItem: () => null,
      setItem() { throw new Error("quota"); },
    };
    expect(() => savePanelSizes(hostile, DEFAULT_PANEL_SIZES)).not.toThrow();
  });
});
