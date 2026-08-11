import { expect, test } from "@playwright/test";
import { boot, gridRows, SIZES } from "./support";

/**
 * The shell's geometry, at both target sizes.
 *
 * jsdom has no layout engine: it cannot tell three grid rows from four, cannot
 * report a resolved track height, and cannot notice a region overflowing the
 * viewport. Everything here needs a real one.
 */

for (const [name, size] of Object.entries(SIZES)) {
  test.describe(`shell layout at ${size.width}×${size.height}`, () => {
    test.use({ viewport: size });

    test("the shell resolves to exactly three grid rows that fill the height", async ({ page }) => {
      // Browser property: computed grid-track resolution. A fourth in-flow child
      // would add an implicit track — the bug shell.test guards in jsdom by
      // counting children, but only a real engine proves the tracks themselves.
      await boot(page);
      const rows = await gridRows(page);
      expect(rows).toHaveLength(3);
      const sum = rows.reduce((a, b) => a + b, 0);
      expect(Math.abs(sum - size.height)).toBeLessThanOrEqual(1);
    });

    test("no region takes an implicit grid row: only three children are in flow", async ({ page }) => {
      // Browser property: `position: fixed` removes an element from grid flow.
      // The mount-card banner is a fixed overlay; a real engine confirms it does
      // not consume a track, which jsdom cannot evaluate.
      await boot(page);
      const inFlow = await page.evaluate(() => {
        const shell = document.querySelector(".app-shell")!;
        return [...shell.children]
          .filter((child) => getComputedStyle(child).position !== "fixed")
          .map((child) => child.className.split(" ")[0]);
      });
      expect(inFlow).toEqual(["topbar", "workbench", "story-ribbon"]);
    });

    test("the page does not scroll horizontally", async ({ page }) => {
      // Browser property: real overflow. `body { overflow: hidden }` plus the
      // min-width floor should never produce a horizontal scrollbar at a target
      // size; jsdom reports scrollWidth === clientWidth regardless.
      await boot(page);
      const overflows = await page.evaluate(
        () => document.documentElement.scrollWidth > document.documentElement.clientWidth,
      );
      expect(overflows).toBe(false);
    });

    test("the workbench holds its three columns with the queue and rail fixed", async ({ page }) => {
      // Browser property: resolved `grid-template-columns`. The queue (300) and
      // judgment rail (400) are fixed; the viewer takes the slack. Two splitter
      // tracks sit between them.
      await boot(page);
      const cols = await page.evaluate(() =>
        getComputedStyle(document.querySelector(".workbench")!)
          .gridTemplateColumns.split(" ")
          .map((value) => Math.round(parseFloat(value))),
      );
      expect(cols).toHaveLength(5);
      expect(cols[0]).toBe(300);
      expect(cols[4]).toBe(400);
      // The viewer column absorbs the width beyond the two fixed rails and the
      // two splitter tracks, so it grows with the window.
      expect(cols[2]).toBe(size.width - 300 - 400 - cols[1] - cols[3]);
    });
  });
}
