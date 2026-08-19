import { expect, test } from "@playwright/test";
import { boot } from "./support";

test.describe("camera lane merge", () => {
  test.use({ viewport: { width: 980, height: 680 } });

  test("previews a merge, keeps focus contained, and converges Review with Chronology", async ({ page }) => {
    await boot(page);
    const sourceActions = page.getByRole("button", { name: "Camera actions for Insta360 X5" });
    await sourceActions.click();
    const actions = page.getByRole("group", { name: "Actions for Insta360 X5" });
    await expect(actions.getByRole("button", { name: "Merge with…" })).toBeVisible();
    await actions.getByRole("button", { name: "Merge with…" }).click();

    const dialog = page.getByRole("dialog", { name: "Merge camera lanes" });
    await expect(dialog).toBeVisible();
    const destination = dialog.getByLabel("Merge with…");
    await expect(destination).toBeFocused();
    await destination.selectOption("DJI Pocket 3");
    await expect(dialog.getByText("Clips moving").locator("..").getByRole("definition")).not.toHaveText("0");
    await expect(dialog.getByText(/cards stay separate/i)).toBeVisible();

    const bounds = await dialog.boundingBox();
    expect(bounds).not.toBeNull();
    expect(bounds!.x).toBeGreaterThanOrEqual(0);
    expect(bounds!.y).toBeGreaterThanOrEqual(0);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(980);
    expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(680);

    await dialog.getByRole("button", { name: "Merge camera" }).click();
    await expect(dialog).toBeHidden();
    await expect(page.getByRole("button", { name: "Camera actions for Insta360 X5" })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Camera actions for DJI Pocket 3" })).toHaveCount(1);

    await page.getByRole("button", { name: "Chronology", exact: true }).click();
    const desk = page.getByRole("region", { name: "Chronology calibration" });
    await expect(desk.getByLabel("Chronology summary")).toContainText("3 cameras");
    await expect(desk.getByLabel("Chronology summary")).toContainText("5 cards");
  });
});
