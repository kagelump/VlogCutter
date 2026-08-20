import { expect, test, type Page } from "@playwright/test";
import { boot, showImportStage, SIZES, startImport } from "./support";

/**
 * The shell stays inside the window while an import runs (#68).
 *
 * Measured, because this bug is invisible to jsdom: the DOM was correct the
 * whole time. `.app-shell` was a grid with an implicit `auto` column, grid
 * items default to `min-width: auto`, and an active import put ~500px of
 * progress text into the top bar. The column sized itself to that min-content
 * — 1633px at a 1280px window — and every row inherited it, so the Story
 * Ribbon hung 353px outside the screen. Only a real layout engine sees that.
 */

interface Containment {
  viewport: number;
  bodyOverflow: number;
  shellOverflow: number;
  ribbonRight: number;
  storyScroll: { client: number; scroll: number } | null;
  slot: string;
}

async function containment(page: Page): Promise<Containment> {
  return page.evaluate(() => {
    const shell = document.querySelector(".app-shell")!;
    const ribbon = document.querySelector(".story-ribbon");
    const scroll = document.querySelector(".story-scroll");
    return {
      viewport: window.innerWidth,
      bodyOverflow: document.body.scrollWidth - document.body.clientWidth,
      shellOverflow: shell.scrollWidth - shell.clientWidth,
      ribbonRight: ribbon ? Math.round(ribbon.getBoundingClientRect().right) : 0,
      storyScroll: scroll ? { client: scroll.clientWidth, scroll: scroll.scrollWidth } : null,
      slot: document.querySelector(".operation-slot")?.textContent ?? "",
    };
  });
}

function expectContained(found: Containment) {
  expect(found.bodyOverflow, "body must not scroll horizontally").toBeLessThanOrEqual(0);
  expect(found.shellOverflow, "the app shell must fit the window").toBeLessThanOrEqual(0);
  expect(found.ribbonRight, "the ribbon must not extend past the window").toBeLessThanOrEqual(found.viewport);
}

for (const [, size] of Object.entries(SIZES)) {
  test.describe(`import containment at ${size.width}×${size.height}`, () => {
    test.use({ viewport: size });

    test("the shell and the ribbon stay inside the window while copying", async ({ page }) => {
      await boot(page);
      expectContained(await containment(page));

      await startImport(page);
      const during = await containment(page);
      expectContained(during);
      expect(during.slot).not.toBe("");
      // The ribbon owns its own horizontal scrolling and nothing else does.
      expect(during.storyScroll).not.toBeNull();
    });

    test("no import stage, and no filename length, can widen the shell", async ({ page }) => {
      await boot(page);
      await startImport(page);

      const longFilename = `${"A".repeat(180)}_C0001.MP4`;
      for (const stage of ["scanning", "copying", "reading", "proxies"]) {
        await showImportStage(page, stage, longFilename);
        expectContained(await containment(page));
      }

      // And the completed/error tail, which swaps the whole slot's contents.
      await expect(page.locator(".operation-slot")).toBeVisible();
      await page.waitForFunction(() => {
        const slot = document.querySelector(".operation-slot");
        return !slot || !slot.textContent?.includes("Copying");
      }, undefined, { timeout: 15_000 });
      expectContained(await containment(page));
    });
  });
}

test.describe("the containment guarantee itself", () => {
  test.use({ viewport: SIZES.small });

  test("no top-bar content can widen the shell, even content that cannot compress", async ({ page }) => {
    // The tests above prove the top bar compresses. This one proves the shell
    // does not depend on it: an element that refuses to shrink is put into the
    // status slot, which is precisely what a future stage label, a longer ETA,
    // or another control could turn out to be. The shell must still fit, and
    // the ribbon must still be the width of the window.
    await boot(page);
    await page.evaluate(() => {
      const slot = document.querySelector(".operation-slot")!;
      (slot as HTMLElement).style.display = "flex";
      const rigid = document.createElement("span");
      rigid.textContent = "incompressible";
      rigid.style.cssText = "flex:none;min-width:2400px;width:2400px";
      slot.appendChild(rigid);
    });

    const found = await containment(page);
    expectContained(found);
    expect(found.ribbonRight).toBe(found.viewport);
  });
});

test.describe("import containment on a wide window", () => {
  test.use({ viewport: { width: 1920, height: 1080 } });

  test("holds with a canvas far wider than the window, at maximum zoom", async ({ page }) => {
    // Enough clips that the chronology canvas genuinely exceeds a 1920px
    // window — the case `story-scroll` exists for, and the one where a shell
    // that took its width from its content would be worst.
    await page.goto("/?clips=600");
    await expect(page.getByRole("listbox", { name: "Clips" })).toBeVisible();
    await expect(page.locator('.queue-list [role="option"]').first()).toBeVisible();

    const zoomIn = page.locator('.zoom-control button[aria-label="Zoom story ribbon in"]');
    for (let press = 0; press < 12; press += 1) {
      if (await zoomIn.isDisabled()) break;
      await zoomIn.click();
    }

    const found = await containment(page);
    expectContained(found);
    expect(found.storyScroll!.scroll, "the canvas is wider than its scroller").toBeGreaterThan(
      found.storyScroll!.client,
    );
  });
});
