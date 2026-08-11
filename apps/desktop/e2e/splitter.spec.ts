import { expect, test } from "@playwright/test";
import { boot } from "./support";

/**
 * The panel splitters, under real pointer input.
 *
 * The Splitter uses pointer capture so a fast drag that outruns the cursor
 * keeps resizing. jsdom has no pointer-capture, no pointer coordinates, and no
 * layout to resize — a component test can call `onResize` directly but cannot
 * prove the handle turns a drag into a new column width. A real engine can.
 */

const queueSplitter = (page: import("@playwright/test").Page) =>
  page.getByRole("separator", { name: "Resize the clip queue" });

async function valueNow(page: import("@playwright/test").Page): Promise<number> {
  return Number(await queueSplitter(page).getAttribute("aria-valuenow"));
}

async function queueWidth(page: import("@playwright/test").Page): Promise<number> {
  return page.evaluate(() => Math.round(document.querySelector(".queue-panel")!.getBoundingClientRect().width));
}

test("dragging the queue splitter widens the queue column by the drag distance", async ({ page }) => {
  // Browser property: pointer capture turning a real drag into a column width.
  await boot(page, "large");
  const before = await valueNow(page);
  expect(await queueWidth(page)).toBe(before);

  const box = (await queueSplitter(page).boundingBox())!;
  const cy = box.y + box.height / 2;
  const cx = box.x + box.width / 2;
  await page.mouse.move(cx, cy);
  await page.mouse.down();
  await page.mouse.move(cx + 80, cy, { steps: 10 });
  await page.mouse.up();

  const after = await valueNow(page);
  expect(after).toBeGreaterThanOrEqual(before + 76);
  expect(after).toBeLessThanOrEqual(before + 84);
  // The reported value and the real column width stay in step.
  expect(await queueWidth(page)).toBe(after);
});

test("double-clicking the splitter restores the default queue width", async ({ page }) => {
  // Browser property: the dblclick reset resolving back to the layout default.
  await boot(page, "large");
  const original = await valueNow(page);

  const box = (await queueSplitter(page).boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 90, box.y + box.height / 2, { steps: 10 });
  await page.mouse.up();
  expect(await valueNow(page)).not.toBe(original);

  await queueSplitter(page).dblclick();
  expect(await valueNow(page)).toBe(original);
  expect(await queueWidth(page)).toBe(original);
});

test("the splitter is a keyboard-operable separator: an arrow key nudges the width", async ({ page }) => {
  // Browser property: real focus on the separator and a keydown that resizes.
  await boot(page, "large");
  await queueSplitter(page).focus();
  await expect(queueSplitter(page)).toBeFocused();
  const before = await valueNow(page);

  await page.keyboard.press("ArrowRight");
  expect(await valueNow(page)).toBe(before + 16);
});
