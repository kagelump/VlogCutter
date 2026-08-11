import { expect, type Page } from "@playwright/test";

/** The two window sizes the desktop layout is specified against. */
export const SIZES = {
  small: { width: 1280, height: 800 },
  large: { width: 1440, height: 900 },
} as const;

/**
 * Boot the preview and wait until the review queue is on screen.
 *
 * The preview installs the CUJ harness in `main.tsx` when Tauri is absent, so
 * the app is the real `App` answering real adapter calls from the same
 * in-memory backend a CUJ uses — only the engine underneath is a real browser.
 */
export async function boot(page: Page, size?: keyof typeof SIZES) {
  // Only override the viewport when a size is asked for; layout specs set theirs
  // through `test.use({ viewport })`, and boot must not stamp over that.
  if (size) await page.setViewportSize(SIZES[size]);
  await page.goto("/");
  await expect(page.getByRole("listbox", { name: "Clips" })).toBeVisible();
  await expect(page.locator('.queue-list [role="option"]').first()).toBeVisible();
}

/** The `.app-shell`'s resolved grid rows, in pixels. */
export async function gridRows(page: Page): Promise<number[]> {
  return page.evaluate(() => {
    const shell = document.querySelector(".app-shell")!;
    return getComputedStyle(shell)
      .gridTemplateRows.split(" ")
      .map((value) => Math.round(parseFloat(value)));
  });
}
