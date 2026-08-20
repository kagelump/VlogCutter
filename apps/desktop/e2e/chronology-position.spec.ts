import { expect, test } from "@playwright/test";
import { boot, SIZES } from "./support";

/**
 * The selected-position readout, measured (#72).
 *
 * jsdom has no layout engine, so it cannot tell a clipped string from a whole
 * one: `overflow: hidden` plus `text-overflow: ellipsis` leaves `textContent`
 * intact while the pixels say `U…`. Only a real engine can prove the timezone
 * is legible, which is exactly what the bug was about.
 */

/** Whether an element's own box is narrower than the text inside it. */
async function isClipped(page: import("@playwright/test").Page, selector: string) {
  return page.evaluate((css) => {
    const node = document.querySelector(css);
    if (!node) return null;
    // 1px of tolerance for sub-pixel text measurement.
    return node.scrollWidth - node.clientWidth > 1;
  }, selector);
}

/** Select a clip whose reading is a full date, time, and IANA zone. */
async function selectFirstClip(page: import("@playwright/test").Page) {
  await page.locator('.queue-list [role="option"]').first().click();
  await expect(page.locator(".chronology-position .position-clock")).toBeVisible();
}

for (const [, size] of Object.entries(SIZES)) {
  test.describe(`selected position at ${size.width}×${size.height}`, () => {
    test.use({ viewport: size });

    test("shows ordinal, full timestamp, and timezone without truncating any of them", async ({ page }) => {
      await boot(page);
      await selectFirstClip(page);

      // The screenshot in the issue: "#32 · Aug 8, 09:21:23 · U…". Every part
      // has to survive at an ordinary width.
      expect(await isClipped(page, ".chronology-position .position-ordinal")).toBe(false);
      expect(await isClipped(page, ".chronology-position .position-date")).toBe(false);
      expect(await isClipped(page, ".chronology-position .position-clock")).toBe(false);
      expect(await isClipped(page, ".chronology-position .position-zone")).toBe(false);

      const zone = page.locator(".chronology-position .position-zone");
      await expect(zone).toBeVisible();
      // An ellipsis anywhere in the zone is the exact defect: an abbreviated
      // zone cannot answer whether the clock beside it is UTC or local.
      await expect(zone).not.toContainText("…");
    });

    test("the readout is not force-clipped by a fixed width", async ({ page }) => {
      await boot(page);
      await selectFirstClip(page);
      const maxWidth = await page.evaluate(
        () => getComputedStyle(document.querySelector(".chronology-position")!).maxWidth,
      );
      expect(maxWidth).toBe("none");
    });
  });
}

test.describe("the readout's compression order", () => {
  test.use({ viewport: { width: 880, height: 800 } });

  test("yields the zoom slider and the ordinal before the timezone", async ({ page }) => {
    await boot(page);
    await selectFirstClip(page);

    // Least useful first: the slider goes while its ± buttons still work.
    await expect(page.locator(".zoom-control input")).toBeHidden();
    await expect(page.locator(".chronology-position .position-ordinal")).toBeHidden();
    // The clock never truncates, at any width: a clipped wall clock beside a
    // whole zone is the original defect wearing its other half.
    expect(await isClipped(page, ".chronology-position .position-clock")).toBe(false);
    // The zone never goes, at any width.
    const zone = page.locator(".chronology-position .position-zone");
    await expect(zone).toBeVisible();
    expect(await isClipped(page, ".chronology-position .position-zone")).toBe(false);
    await expect(page.locator('.zoom-control button[aria-label="Zoom story ribbon in"]')).toBeVisible();
  });
});
