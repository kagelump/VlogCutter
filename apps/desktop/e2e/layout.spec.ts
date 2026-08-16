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

test.describe("the queue's offline mark", () => {
  test.use({ viewport: SIZES.small });

  test("does not change a row's height or push its content out of the row", async ({ page }) => {
    // Browser property: real box measurement. The queue is virtualized against
    // a fixed 52px row and read by position at speed, so a mark that grew a row
    // would both misplace every row below it and cost the recognizability the
    // fixed geometry buys. jsdom reports every height as zero.
    await boot(page);
    const rows = await page.evaluate(() => {
      const marked: number[] = [];
      const plain: number[] = [];
      let clipped = false;
      for (const row of document.querySelectorAll<HTMLElement>('.queue-list [role="option"]')) {
        (row.querySelector(".offline-pip") ? marked : plain).push(row.getBoundingClientRect().height);
        const pip = row.querySelector<HTMLElement>(".offline-pip");
        if (pip) {
          const rowBox = row.getBoundingClientRect();
          const pipBox = pip.getBoundingClientRect();
          if (pipBox.right > rowBox.right + 0.5 || pipBox.bottom > rowBox.bottom + 0.5) clipped = true;
        }
      }
      return { marked, plain, clipped };
    });

    expect(rows.marked.length).toBeGreaterThan(0);
    expect(rows.plain.length).toBeGreaterThan(0);
    expect(new Set([...rows.marked, ...rows.plain].map(Math.round))).toEqual(new Set([52]));
    expect(rows.clipped).toBe(false);
  });
});

test.describe("the label typeahead", () => {
  test.use({ viewport: SIZES.small });

  test("floats over the rail instead of moving the notes field", async ({ page }) => {
    // Browser property: real box positions. Rule 3 says nothing may move the
    // notes field, and a suggestion list laid out in flow would push
    // everything below it down. jsdom reports every rectangle as zero, so it
    // cannot tell an overlay from a block that grew the rail.
    await boot(page);
    const before = await page.locator("#notes").boundingBox();

    await page.locator("#label-input").fill("a");
    const list = page.locator(".label-vocabulary");
    await expect(list).toBeVisible();

    expect(await page.locator("#notes").boundingBox()).toEqual(before);
    const listBox = (await list.boundingBox())!;
    const fieldBox = (await page.locator("#label-input").boundingBox())!;
    expect(listBox.y).toBeGreaterThanOrEqual(fieldBox.y + fieldBox.height - 1);
    expect(listBox.width).toBeLessThanOrEqual(fieldBox.width + 1);
  });
});
