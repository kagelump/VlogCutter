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

/**
 * Start a Smart ingest run from the mount banner and leave it running.
 *
 * The preview releases one item at a time on a real timer, so the operation
 * slot is genuinely live rather than a styled fixture.
 */
export async function startImport(page: Page) {
  await page.getByRole("button", { name: "Review import" }).click();
  const dialog = page.getByRole("dialog");
  const start = dialog.getByRole("button", { name: /^Import \d+ clips?$/ });
  await expect(start).toBeEnabled();
  await start.click();
  await expect(page.locator(".operation-slot")).toContainText(/Copying|Importing|Reading|proxies|\d+\/\d+/);
}

/**
 * One import stage, pushed through the real event transport rather than by
 * styling a fixture — including, where a caller asks for it, a filename far
 * longer than any card would produce.
 */
export async function showImportStage(
  page: Page,
  stage: string,
  currentFile: string,
  counts: { completed: number; total: number } = { completed: 7, total: 243 },
) {
  await page.evaluate(async ([importStage, file, completed, total]) => {
    const harness = (window as unknown as { __RCDVLOG_PREVIEW__: {
      state: { settings: { lastProjectId?: string }; projects: Array<{ id: string }> };
      emit(event: string, payload: unknown): Promise<number>;
    } }).__RCDVLOG_PREVIEW__;
    const projectId = harness.state.settings.lastProjectId ?? harness.state.projects[0].id;
    await harness.emit("import-progress", {
      projectId,
      stage: importStage,
      completed,
      total,
      currentFile: file,
      completedBytes: 7_000_000_000,
      totalBytes: 243_000_000_000,
      asset: null,
    });
  }, [stage, currentFile, counts.completed, counts.total] as const);
}
