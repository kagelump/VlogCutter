import { expect, test, type Page } from "@playwright/test";
import { boot, showImportStage, startImport } from "./support";

/**
 * A busy top bar degrades instead of clipping its own controls (#75).
 *
 * #68 stopped an import widening the shell by clipping the top bar. The bar
 * then clipped itself: every child of the operation slot is `flex: none`, so
 * once the label and the ETA had ellipsized away the slot went on shrinking
 * and cut the Stop button in half — the one control a user needs while an
 * import is running.
 *
 * Only a real layout engine sees this. jsdom reports the same DOM either way:
 * the markup was never wrong, the boxes were. So every assertion here is
 * geometry — is this control inside the bar, and does a click at its centre
 * actually reach it — measured at each window size the layout is specified
 * against, in each stage the bar can be in.
 */

/** The controls the ladder may never collapse, shrink, or clip. */
const NEVER_GIVE = [
  { name: "the progress count", selector: ".progress-readout output" },
  { name: "the cancel button", selector: ".progress-cancel" },
  { name: "Add media", selector: ".topbar > .button.primary" },
  { name: "the project switcher", selector: ".project-switcher" },
];

interface Placement {
  found: boolean;
  /** Fully inside the top bar's own box, so nothing of it is clipped away. */
  contained: boolean;
  /** What a click at the control's centre would land on. */
  hit: "self" | "elsewhere" | "nothing";
  width: number;
}

async function place(page: Page, selector: string): Promise<Placement> {
  return page.evaluate((target) => {
    const bar = document.querySelector(".topbar")!.getBoundingClientRect();
    const node = document.querySelector(target);
    if (!node) return { found: false, contained: false, hit: "nothing" as const, width: 0 };
    const box = node.getBoundingClientRect();
    const hit = document.elementFromPoint((box.left + box.right) / 2, (box.top + box.bottom) / 2);
    return {
      found: true,
      // Half a pixel of slack: sub-pixel layout, not a clipped control.
      contained: box.left >= bar.left - 0.5 && box.right <= bar.right + 0.5
        && box.top >= bar.top - 0.5 && box.bottom <= bar.bottom + 0.5,
      hit: hit === null ? "nothing" as const : node.contains(hit) ? "self" as const : "elsewhere" as const,
      width: box.width,
    };
  }, selector);
}

async function shellOverflow(page: Page): Promise<number> {
  return page.evaluate(() => {
    const shell = document.querySelector(".app-shell")!;
    return shell.scrollWidth - shell.clientWidth;
  });
}

/** Every never-give control, whole and reachable, with the shell still contained. */
async function expectNothingGivesWay(page: Page, where: string) {
  expect(await shellOverflow(page), `the shell must fit the window ${where}`).toBeLessThanOrEqual(0);
  for (const control of NEVER_GIVE) {
    const placement = await place(page, control.selector);
    expect(placement.found, `${control.name} must be rendered ${where}`).toBe(true);
    expect(placement.width, `${control.name} must have width ${where}`).toBeGreaterThan(0);
    expect(placement.contained, `${control.name} must not be clipped ${where}`).toBe(true);
    expect(placement.hit, `${control.name} must be clickable ${where}`).toBe("self");
  }
}

/** Windows the layout is specified against, plus the reported wide screenshot. */
const WINDOWS = [
  { width: 980, height: 680 },
  { width: 1280, height: 800 },
  { width: 1440, height: 900 },
  { width: 1920, height: 1080 },
];

/** A filename far longer than any card would produce. */
const LONG_FILENAME = `${"A".repeat(180)}_C0001.MP4`;

for (const size of WINDOWS) {
  test.describe(`a busy top bar at ${size.width}×${size.height}`, () => {
    test.use({ viewport: size });

    test("keeps every never-give control whole through every import stage", async ({ page }) => {
      await boot(page);
      await startImport(page);

      for (const stage of ["scanning", "copying", "reading", "proxies"]) {
        // Four digits either side, which is the widest the count can get for
        // the ~thousands of clips this app is sized for.
        await showImportStage(page, stage, LONG_FILENAME, { completed: 1097, total: 1953 });
        await expectNothingGivesWay(page, `while ${stage} at ${size.width}px`);
      }
    });

    test("keeps the analysis Cancel button whole, cost line and all", async ({ page }) => {
      await boot(page);
      await page.getByLabel("More media generation actions").click();
      await page.getByRole("button", { name: /Analyze \d+ clips/ }).click();
      const dialog = page.getByRole("dialog");
      await dialog.getByRole("button", { name: "Start" }).click();
      await expect(page.locator(".progress-cancel")).toBeVisible();

      await expectNothingGivesWay(page, `while analysing at ${size.width}px`);
    });
  });
}

test.describe("the ladder's order", () => {
  test.use({ viewport: { width: 1440, height: 900 } });

  test("spends the wordmark and the lifecycle labels before the mode switcher", async ({ page }) => {
    await boot(page);
    const shown = (selector: string) => page.evaluate((target) => {
      const node = document.querySelector(target);
      return node ? getComputedStyle(node).display !== "none" : false;
    }, selector);

    // Idle at 1440 there is room for everything.
    expect(await shown(".wordmark > span:not(.wordmark-mark)")).toBe(true);
    expect(await shown(".topbar > .desktop-lifecycle-action")).toBe(true);
    expect(await shown(".topbar-overflow")).toBe(false);

    // An operation claims the same row, so the cheap collapses happen at a
    // width that idle would not have needed them at.
    await startImport(page);
    expect(await shown(".wordmark > span:not(.wordmark-mark)")).toBe(false);
    expect(await shown(".topbar > .desktop-lifecycle-action")).toBe(false);
    expect(await shown(".topbar-overflow")).toBe(true);
    // …and the switcher, which costs the user more, still has its words.
    expect(await shown(".mode-switcher .mode-label")).toBe(true);
    await expectNothingGivesWay(page, "while importing at 1440px");
  });

  test("falls back to mode glyphs only once the cheaper collapses are spent", async ({ page }) => {
    await boot(page);
    await startImport(page);
    await page.setViewportSize({ width: 1000, height: 800 });
    await showImportStage(page, "copying", LONG_FILENAME);

    expect(await page.locator(".mode-switcher .mode-label").first().isVisible()).toBe(false);
    // The targets are still there — three buttons, still named, still pressable.
    const names = await page.locator(".mode-switcher button").evaluateAll(
      (nodes) => nodes.map((node) => node.getAttribute("aria-label")),
    );
    expect(names).toEqual(["Review", "Chronology", "Edit plan"]);
    await expectNothingGivesWay(page, "while importing at 1000px");
  });
});

test.describe("the compact progress form", () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test("is the count with a rule beneath it, and keeps the rest on hover", async ({ page }) => {
    await boot(page);
    await startImport(page);
    await showImportStage(page, "proxies", "C0097.MP4", { completed: 97, total: 1953 });

    const shape = await page.evaluate(() => {
      const count = document.querySelector(".progress-readout output")!.getBoundingClientRect();
      const track = document.querySelector(".progress-track")!.getBoundingClientRect();
      const label = document.querySelector(".operation-label")!;
      const detail = document.querySelector(".operation-detail")!;
      return {
        labelShown: getComputedStyle(label).display !== "none",
        detailShown: getComputedStyle(detail).display !== "none",
        ruleBelowCount: track.top >= count.bottom - 0.5,
        ruleMatchesCount: Math.abs(track.width - count.width) <= 1,
        tooltip: document.querySelector(".operation-progress")!.getAttribute("title") ?? "",
      };
    });

    expect(shape.labelShown, "the label is what the compact form gives up").toBe(false);
    expect(shape.detailShown, "so is the ETA and worker line").toBe(false);
    expect(shape.ruleBelowCount).toBe(true);
    expect(shape.ruleMatchesCount).toBe(true);
    // Nothing is lost: it moved to the tooltip.
    expect(shape.tooltip).toContain("Making import proxies");
    expect(shape.tooltip).toContain("97/1953");
    expect(shape.tooltip).toContain("C0097.MP4");
    expect(shape.tooltip).toContain("worker");
  });

  test("gives way to the full form when the window has room for it", async ({ page }) => {
    await boot(page);
    await page.setViewportSize({ width: 1920, height: 1080 });
    await startImport(page);
    await showImportStage(page, "copying", "C0097.MP4");

    const full = await page.evaluate(() => ({
      label: document.querySelector(".operation-label")!.textContent,
      detailShown: getComputedStyle(document.querySelector(".operation-detail")!).display !== "none",
      trackWidth: document.querySelector(".progress-track")!.getBoundingClientRect().width,
    }));
    expect(full.label).toBe("Copying and verifying originals");
    expect(full.detailShown).toBe(true);
    expect(full.trackWidth).toBe(160);
  });
});

test.describe("the bar's own menus", () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  /**
   * The ladder routes controls into `•••`, so the menus have to survive the
   * containment that started all this. `overflow: hidden` (#68) clipped both
   * of the bar's dropdowns the moment they opened — they hang below a 44px
   * row — and layout containment from the container query would have left
   * them painting under the workbench.
   */
  test("open below the bar rather than being clipped or painted over", async ({ page }) => {
    await boot(page);
    await startImport(page);

    await page.locator(".media-split-toggle").click();
    const media = await page.evaluate(() => {
      const menu = document.querySelector(".media-menu")!;
      const box = menu.getBoundingClientRect();
      const hit = document.elementFromPoint(box.left + box.width / 2, box.top + 20);
      return { belowTheBar: box.bottom > 44, reached: hit !== null && menu.contains(hit) };
    });
    expect(media.belowTheBar).toBe(true);
    expect(media.reached, "the media menu must be hittable outside the bar's box").toBe(true);
    await page.keyboard.press("Escape");

    // Narrow enough that ••• is on screen whether or not the import is still
    // running, so this measures the menu rather than the ladder's timing.
    await page.setViewportSize({ width: 1000, height: 800 });
    await page.locator(".topbar-overflow > button").click();
    const overflow = await page.evaluate(() => {
      const menu = document.querySelector(".topbar-overflow > div")!;
      const box = menu.getBoundingClientRect();
      const hit = document.elementFromPoint(box.left + box.width / 2, box.top + 20);
      return { belowTheBar: box.bottom > 44, reached: hit !== null && menu.contains(hit) };
    });
    expect(overflow.belowTheBar).toBe(true);
    expect(overflow.reached, "the ••• menu must be hittable outside the bar's box").toBe(true);
    // The lifecycle actions the ladder collapsed are all in there.
    for (const action of ["Open catalog", "Settings", "New project", "Keyboard shortcuts"]) {
      await expect(page.locator(".topbar-overflow > div").getByRole("button", { name: action })).toBeVisible();
    }
  });

  test("cannot be widened past the bar by a long project name", async ({ page }) => {
    await boot(page);
    await startImport(page);
    // A `select` sizes itself to its widest option, which is the one piece of
    // top-bar width the user supplies directly.
    await page.evaluate(() => {
      const option = document.querySelector(".project-switcher option")!;
      option.textContent = `Hokkaido ${"very ".repeat(40)}long project`;
    });
    await showImportStage(page, "copying", LONG_FILENAME);

    await expectNothingGivesWay(page, "with a 200-character project name");
  });
});
