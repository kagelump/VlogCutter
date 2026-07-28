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

test("Chrome administration loads durable state and records a root mutation", async ({ page }) => {
  await page.setViewportSize({ width: 980, height: 680 });
  const bootstrap = await page.request.get("/api/v1/auth/bootstrap");
  expect(bootstrap.ok()).toBeTruthy();
  const { code } = await bootstrap.json() as { code: string };
  await page.goto("/");
  await page.getByLabel("Bootstrap code").fill(code);
  await page.getByRole("button", { name: "Open review desk" }).click();
  await page.getByRole("link", { name: "Administration" }).click();
  await expect(page.getByRole("heading", { name: "Administration" })).toBeVisible();
  await expect(page.getByText("All prerequisites ready")).toBeVisible();
  await page.getByLabel("Logical root name").fill("Chrome archive");
  await page.getByRole("button", { name: "Register root" }).click();
  await expect(page.locator(".admin-table strong", { hasText: "Chrome archive" }).first()).toBeVisible();
  await expect(page.getByText("storage_root.created")).toBeVisible();
});
