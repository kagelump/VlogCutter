import { expect, test } from "@playwright/test";
import { SIZES } from "./support";

/**
 * What the two review surfaces cost to mount at the scale the product targets.
 *
 * AGENTS.md §1: thousands of mostly 4K/60 clips. The queue and the story ribbon
 * both position fixed-geometry children absolutely and window them, and the
 * property that makes that worth having — the DOM stays bounded while the
 * project grows — is exactly the one jsdom cannot check, because it has no
 * viewport to measure the window against and would render everything.
 *
 * These are budgets, not benchmarks. The numbers are loose enough not to fail
 * on a slow CI box and tight enough that losing the windowing fails them.
 */

const CLIPS = 3_000;

test.use({ viewport: SIZES.small });

async function bootAtScale(page: import("@playwright/test").Page) {
  await page.goto(`/?clips=${CLIPS}`);
  await expect(page.getByRole("listbox", { name: "Clips" })).toBeVisible();
  await expect(page.locator('.queue-list [role="option"]').first()).toBeVisible();
}

test("mounts a bounded queue and ribbon for a three-thousand-clip project", async ({ page }) => {
  await bootAtScale(page);

  const counts = await page.evaluate(() => ({
    rows: document.querySelectorAll('.queue-list [role="option"]').length,
    cells: document.querySelectorAll(".lane-track .timeline-clip").length,
    total: document.querySelectorAll("*").length,
  }));

  // A 1280×800 window shows ~13 queue rows; the window plus its overscan is
  // dozens, not thousands. Without windowing this is 3,000.
  expect(counts.rows).toBeGreaterThan(0);
  expect(counts.rows).toBeLessThan(120);
  // The ribbon draws every clip once per camera lane, so unwindowed it is
  // 3,000 cells across five lanes.
  expect(counts.cells).toBeGreaterThan(0);
  expect(counts.cells).toBeLessThan(600);
  expect(counts.total).toBeLessThan(4_000);
});

test("keeps the whole queue reachable: the last clip is the three-thousandth", async ({ page }) => {
  await bootAtScale(page);

  // Windowing must not cost the list its extent. The scroller is sized for the
  // whole project even though only a slice of it is mounted.
  const scrollHeight = await page.locator(".queue-list").evaluate((list) => list.scrollHeight);
  expect(scrollHeight).toBeGreaterThan(CLIPS * 40);

  await page.locator(".queue-list").evaluate((list) => { list.scrollTop = list.scrollHeight; });
  await expect(page.locator('.queue-list [role="option"]').last()).toContainText(`${CLIPS}`);
});

test("answers a search keystroke without mounting the results it filtered to", async ({ page }) => {
  await bootAtScale(page);
  const search = page.getByPlaceholder("Filter titles, labels, notes, speech…");

  const started = Date.now();
  await search.fill("SONY");
  await expect(page.locator(".queue-panel .count-badge")).not.toHaveText(String(CLIPS));
  const elapsed = Date.now() - started;

  // Generous, because this is a budget rather than a benchmark: what it fails
  // on is a keystroke that re-mounts hundreds of rows, not a slow machine.
  expect(elapsed).toBeLessThan(3_000);
  expect(await page.locator('.queue-list [role="option"]').count()).toBeLessThan(120);
});
