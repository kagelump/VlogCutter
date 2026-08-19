import { expect, test } from "@playwright/test";
import { boot } from "./support";

/**
 * Accessibility-tree semantics and keyboard navigation, in a real engine.
 *
 * Playwright resolves roles and the selected state through Chromium's own
 * accessibility tree, and delivers real key events with a real focus model.
 * A jsdom role query reads the DOM; it does not build the tree a screen reader
 * would, and its focus/tab handling is partial.
 */

test("the review queue is a listbox with one selected option in the accessibility tree", async ({ page }) => {
  // Browser property: the accessibility tree, not just the DOM attributes.
  await boot(page, "large");
  // Scoped to Clips on purpose: the project picker is a listbox of options too.
  const queue = page.getByRole("listbox", { name: "Clips" });
  await expect(queue).toBeVisible();
  await expect(queue.getByRole("option")).not.toHaveCount(0);

  const selected = queue.getByRole("option", { selected: true });
  await expect(selected).toHaveCount(1);
  await expect(selected).toContainText("Train pulls into Otaru station");
});

test("j and k move the selected option through the queue", async ({ page }) => {
  // Browser property: real keydown delivery and single-selection invariant held
  // in the accessibility tree as the selection moves.
  await boot(page, "large");
  const queue = page.getByRole("listbox", { name: "Clips" });
  const selected = () => queue.getByRole("option", { selected: true });
  await expect(selected()).toHaveCount(1);
  const first = (await selected().textContent())!;

  // Order-agnostic on purpose: the assertion is that the selection moves and
  // stays single, not which demo clip is where.
  await page.keyboard.press("j");
  await expect(selected()).toHaveCount(1);
  await expect(selected()).not.toHaveText(first);

  await page.keyboard.press("k");
  await expect(selected()).toHaveCount(1);
  await expect(selected()).toHaveText(first);
});

test("Tab traverses real focusable controls rather than dropping focus to the body", async ({ page }) => {
  // Browser property: sequential focus navigation. jsdom does not implement the
  // browser's tab order; here every Tab lands on a genuinely focusable control.
  await boot(page, "large");
  await page.getByPlaceholder("Filter titles, labels, notes, speech…").focus();

  const landed: string[] = [];
  for (let i = 0; i < 5; i += 1) {
    await page.keyboard.press("Tab");
    landed.push(
      await page.evaluate(() => {
        const el = document.activeElement;
        if (!el || el === document.body) return "BODY";
        return (el.getAttribute("role") || el.tagName).toLowerCase();
      }),
    );
  }
  // Focus never fell through to the body; every stop is a real control.
  expect(landed).not.toContain("BODY");
});

test("smart ingest exposes the camera override and focuses a new camera name", async ({ page }) => {
  // Browser property: the real select/input focus handoff and the dialog's
  // accessible control names, which jsdom cannot validate end to end.
  await boot(page, "large");
  await page.getByRole("button", { name: "Review import" }).click();

  const dialog = page.getByRole("dialog");
  const camera = dialog.getByRole("combobox", { name: "Change camera" });
  await expect(camera).toBeVisible();
  await camera.selectOption({ label: "New camera name…" });

  const name = dialog.getByRole("textbox", { name: "New camera name" });
  await expect(name).toBeFocused();
  await name.fill("Sony A7C II");
  await expect(dialog.getByRole("button", { name: "Use this name" })).toBeEnabled();
});
