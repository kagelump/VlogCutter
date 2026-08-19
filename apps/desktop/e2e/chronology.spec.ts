import { expect, test } from "@playwright/test";
import { boot } from "./support";

test.describe("chronology camera lanes", () => {
  test.use({ viewport: { width: 980, height: 680 } });

  test("keeps two Sony cards inside one selectable camera lane", async ({ page }) => {
    // The preview deliberately carries two Sony sequences plus three other
    // cameras, so this is a real-engine check of the hierarchy and its bounds.
    await boot(page);
    await page.getByRole("button", { name: "Chronology", exact: true }).click();
    const desk = page.getByRole("region", { name: "Chronology calibration" });
    await expect(desk).toBeVisible();

    const lanes = desk.locator(".chronology-camera-lane");
    await expect(lanes).toHaveCount(4);
    const sonyLane = lanes.filter({ hasText: "Sony A7C II" });
    await expect(sonyLane).toHaveCount(1);
    await expect(sonyLane.getByText("2 cards")).toBeVisible();
    const cards = sonyLane.getByRole("button", { name: /Inspect card \d of 2.*Sony A7C II · card/ });
    await expect(cards).toHaveCount(4);
    await expect(lanes.filter({ hasText: "DJI Pocket 3" })).toHaveCount(1);

    const boxes = await cards.evaluateAll((buttons) => buttons.map((button) => {
      const box = button.getBoundingClientRect();
      return { left: box.left, right: box.right, top: box.top, bottom: box.bottom };
    }));
    const leftToRight = [...boxes].sort((left, right) => left.left - right.left);
    for (let index = 1; index < leftToRight.length; index += 1) {
      expect(leftToRight[index - 1].right).toBeLessThan(leftToRight[index].left);
    }
    const laneBox = await sonyLane.boundingBox();
    expect(laneBox).not.toBeNull();
    expect(boxes.every((box) => box.top >= laneBox!.y && box.bottom <= laneBox!.y + laneBox!.height))
      .toBe(true);

    const secondCard = sonyLane.getByRole("button", {
      name: /Inspect card 2 of 2, run 1 of 2: Sony A7C II · card B/,
    });
    await secondCard.focus();
    await expect(secondCard).toBeFocused();
    await secondCard.press("Enter");
    const inspector = desk.getByRole("complementary", { name: "Chronology inspector" });
    await expect(inspector.getByRole("heading", { name: "SONY_002.MP4" })).toBeVisible();

    const bounds = await page.evaluate(() => {
      const shell = document.querySelector<HTMLElement>(".app-shell")!.getBoundingClientRect();
      const workspace = document.querySelector<HTMLElement>(".chronology-workspace")!.getBoundingClientRect();
      const scroll = document.querySelector<HTMLElement>(".chronology-canvas-scroll")!;
      return {
        viewport: { width: window.innerWidth, height: window.innerHeight },
        shell: { left: shell.left, top: shell.top, right: shell.right, bottom: shell.bottom },
        workspace: {
          left: workspace.left,
          top: workspace.top,
          right: workspace.right,
          bottom: workspace.bottom,
        },
        pageOverflows: document.documentElement.scrollWidth > document.documentElement.clientWidth
          || document.documentElement.scrollHeight > document.documentElement.clientHeight,
        canvasScrolls: scroll.scrollWidth > scroll.clientWidth,
        canvasOverflowX: getComputedStyle(scroll).overflowX,
      };
    });
    expect(bounds.pageOverflows).toBe(false);
    expect(bounds.shell.left).toBeGreaterThanOrEqual(0);
    expect(bounds.shell.top).toBeGreaterThanOrEqual(0);
    expect(bounds.shell.right).toBeLessThanOrEqual(bounds.viewport.width);
    expect(bounds.shell.bottom).toBeLessThanOrEqual(bounds.viewport.height);
    expect(bounds.workspace.left).toBeGreaterThanOrEqual(bounds.shell.left);
    expect(bounds.workspace.top).toBeGreaterThanOrEqual(bounds.shell.top);
    expect(bounds.workspace.right).toBeLessThanOrEqual(bounds.shell.right);
    expect(bounds.workspace.bottom).toBeLessThanOrEqual(bounds.shell.bottom);
    expect(bounds.canvasScrolls).toBe(true);
    expect(bounds.canvasOverflowX).toBe("auto");
  });
});
