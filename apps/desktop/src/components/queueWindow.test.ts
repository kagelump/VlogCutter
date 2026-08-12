import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  QUEUE_ROW_HEIGHT,
  queueWindow,
  scrollTopForQueueIndex,
} from "./queueWindow";

const css = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "styles.css"), "utf8");

describe("queueWindow", () => {
  it("shares its row height with the visual queue contract", () => {
    const queueItemRule = css.match(/\.queue-item\s*\{([^}]*)\}/)?.[1] ?? "";
    expect(queueItemRule).toContain(`height: ${QUEUE_ROW_HEIGHT}px`);
  });

  it.each([600, 2_000, 10_000])(
    "bounds the mounted slice for a %d-clip project",
    (itemCount) => {
      const result = queueWindow({
        itemCount,
        scrollTop: 4_000 * QUEUE_ROW_HEIGHT,
        viewportHeight: 520,
      });

      expect(result.end - result.start).toBeLessThanOrEqual(22);
      expect(result.totalHeight).toBe(itemCount * QUEUE_ROW_HEIGHT);
      expect(result.start).toBeGreaterThanOrEqual(0);
      expect(result.end).toBeLessThanOrEqual(itemCount);
    },
  );

  it("clamps overscan at both ends without dropping a visible row", () => {
    expect(queueWindow({ itemCount: 100, scrollTop: 0, viewportHeight: 104 }))
      .toEqual({ start: 0, end: 8, totalHeight: 5_200 });
    expect(queueWindow({ itemCount: 100, scrollTop: 99_999, viewportHeight: 104 }))
      .toEqual({ start: 92, end: 100, totalHeight: 5_200 });
  });

  it("renders the complete queue when no layout measurement is available", () => {
    expect(queueWindow({ itemCount: 600, scrollTop: 10_000, viewportHeight: Infinity }))
      .toEqual({ start: 0, end: 600, totalHeight: 31_200 });
  });
});

describe("scrollTopForQueueIndex", () => {
  it("does nothing while the selected row is already visible", () => {
    expect(scrollTopForQueueIndex({
      index: 12,
      itemCount: 1_000,
      scrollTop: 10 * QUEUE_ROW_HEIGHT,
      viewportHeight: 5 * QUEUE_ROW_HEIGHT,
    })).toBe(10 * QUEUE_ROW_HEIGHT);
  });

  it("uses nearest-edge scrolling above and below the viewport", () => {
    expect(scrollTopForQueueIndex({
      index: 3,
      itemCount: 1_000,
      scrollTop: 10 * QUEUE_ROW_HEIGHT,
      viewportHeight: 5 * QUEUE_ROW_HEIGHT,
    })).toBe(3 * QUEUE_ROW_HEIGHT);
    expect(scrollTopForQueueIndex({
      index: 20,
      itemCount: 1_000,
      scrollTop: 10 * QUEUE_ROW_HEIGHT,
      viewportHeight: 5 * QUEUE_ROW_HEIGHT,
    })).toBe(16 * QUEUE_ROW_HEIGHT);
  });

  it("can land on the last row without scrolling past the content", () => {
    expect(scrollTopForQueueIndex({
      index: 9_999,
      itemCount: 10_000,
      scrollTop: 0,
      viewportHeight: 520,
    })).toBe(10_000 * QUEUE_ROW_HEIGHT - 520);
  });
});
