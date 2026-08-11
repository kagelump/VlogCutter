import { expect, test } from "@playwright/test";
import { boot } from "./support";

/**
 * Focus presentation and real scrolling.
 *
 * jsdom stubs `scrollIntoView` to a no-op (see `src/test/setup.ts`) and has no
 * scroll or focus geometry, so none of these can be established in the fast
 * tier. Focus is a CSS presentation over the mounted tree; here a real engine
 * confirms the reflow and that DOM focus and the caret are genuine.
 */

test("entering Focus reflows the shell: the queue hides and the workbench becomes one column", async ({ page }) => {
  // Browser property: computed visibility and grid reflow driven by a class.
  await boot(page, "large");
  await expect(page.locator(".queue-panel")).toBeVisible();

  await page.keyboard.press("f");
  await expect(page.locator(".app-shell")).toHaveClass(/is-focus/);

  await expect(page.locator(".queue-panel")).toBeHidden();
  const columns = await page.evaluate(
    () => getComputedStyle(document.querySelector(".workbench")!).gridTemplateColumns.split(" ").length,
  );
  expect(columns).toBe(1);
});

test("Focus places real DOM focus in the notes field, with a real caret that typing advances", async ({ page }) => {
  // Browser property: authoritative focus management and text-caret position.
  await boot(page, "large");
  await page.keyboard.press("f");
  await expect(page.locator("#notes")).toBeFocused();

  await page.keyboard.type("harbour");
  const caret = await page.locator("#notes").evaluate((el: HTMLTextAreaElement) => ({
    value: el.value,
    selectionStart: el.selectionStart,
  }));
  // The caret lands at offset 0 on entry, so the word prepends and the real
  // caret advances to sit just after it.
  expect(caret.value.startsWith("harbour")).toBe(true);
  expect(caret.selectionStart).toBe("harbour".length);

  // Escape leaves Focus and keeps the paragraph (saving, not discarding).
  await page.keyboard.press("Escape");
  await expect(page.locator(".app-shell")).not.toHaveClass(/is-focus/);
  await expect(page.locator("#notes")).toHaveValue(/harbour/);
});

test("the selected queue row is scrolled into view when the selection moves off-screen", async ({ page }) => {
  // Browser property: real scrolling. The app calls scrollIntoView on selection
  // change; jsdom's stub makes this untestable there.
  await boot(page, "small");
  const list = page.locator(".queue-list");
  await expect(list).toBeVisible();

  // Step to the last clip with the keyboard, so the app (not Playwright's
  // auto-scroll-before-click) is what brings the row into view.
  const rows = page.locator('.queue-list [role="option"]');
  const count = await rows.count();
  for (let i = 0; i < count; i += 1) await page.keyboard.press("j");

  const selected = page.locator('.queue-list [role="option"][aria-selected="true"]');
  await expect(selected).toHaveCount(1);
  const visible = await page.evaluate(() => {
    const listEl = document.querySelector(".queue-list")!;
    const row = document.querySelector('.queue-list [role="option"][aria-selected="true"]')!;
    const l = listEl.getBoundingClientRect();
    const r = row.getBoundingClientRect();
    return { scrolled: listEl.scrollTop > 0, withinTop: r.top >= l.top - 1, withinBottom: r.bottom <= l.bottom + 1 };
  });
  expect(visible.scrolled).toBe(true);
  expect(visible.withinTop).toBe(true);
  expect(visible.withinBottom).toBe(true);
});

test("the body itself never scrolls, on either axis", async ({ page }) => {
  // Browser property: `body { overflow: hidden }` in a real engine. Regions
  // scroll inside their own panels; the page frame does not.
  await boot(page, "small");
  const bodyScrolls = await page.evaluate(() => {
    const doc = document.documentElement;
    return doc.scrollHeight > doc.clientHeight || doc.scrollWidth > doc.clientWidth;
  });
  expect(bodyScrolls).toBe(false);
});
