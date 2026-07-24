import { expect, test } from "@playwright/test";

test("Chrome reaches the real unauthenticated server session flow", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: /Your footage stays close/ })).toBeVisible();
  await expect(page.getByLabel("Bootstrap code")).toBeVisible();
});

test("Chrome can exchange a real one-time code and load the shell", async ({ page }) => {
  const bootstrap = await page.request.get("/api/v1/auth/bootstrap");
  expect(bootstrap.ok()).toBeTruthy();
  const { code } = await bootstrap.json() as { code: string };
  await page.goto("/");
  await page.getByLabel("Bootstrap code").fill(code!);
  await page.getByRole("button", { name: "Open review desk" }).click();
  await expect(page.getByRole("heading", { name: "Review desk ready" })).toBeVisible();
  await expect(page.getByText("Server ready")).toBeVisible();
});
