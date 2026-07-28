import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

function bootstrapCode(index: number): string {
  const configured = process.env.RCDVLOG_E2E_BOOTSTRAP_CODES?.split(",")[index - 1]?.trim();
  return configured || readFileSync(join(tmpdir(), `rcdvlog-web-e2e-bootstrap-${index}.secret`), "utf8").trim();
}

test("Chrome reaches the real unauthenticated server session flow", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: /Your footage stays close/ })).toBeVisible();
  await expect(page.getByLabel("Bootstrap code")).toBeVisible();
});

test("Chrome can exchange a real one-time code and load the shell", async ({ page }) => {
  await page.goto("/");
  await page.getByLabel("Bootstrap code").fill(bootstrapCode(1));
  await page.getByRole("button", { name: "Open review desk" }).click();
  await expect(page.getByRole("heading", { name: "Review desk ready" })).toBeVisible();
  await expect(page.getByText("Server ready")).toBeVisible();
});

test("Chrome administration loads durable state and records a root mutation", async ({ page }) => {
  await page.setViewportSize({ width: 980, height: 680 });
  await page.goto("/");
  await page.getByLabel("Bootstrap code").fill(bootstrapCode(2));
  await page.getByRole("button", { name: "Open review desk" }).click();
  await page.getByRole("link", { name: "Administration" }).click();
  await expect(page.getByRole("heading", { name: "Administration" })).toBeVisible();
  await expect(page.getByText("All prerequisites ready")).toBeVisible();
  await page.getByLabel("Logical root name").fill("Chrome archive");
  await page.getByRole("button", { name: "Register root" }).click();
  await expect(page.locator(".admin-table strong", { hasText: "Chrome archive" }).first()).toBeVisible();
  await expect(page.getByText("storage_root.created")).toBeVisible();
});

test("two Chrome tabs converge concurrent notes when one WebSocket is unavailable", async ({ page, context }) => {
  await page.goto("/");
  await page.getByLabel("Bootstrap code").fill(bootstrapCode(3));
  await page.getByRole("button", { name: "Open review desk" }).click();
  await page.getByRole("link", { name: "Projects" }).click();
  await page.locator(".project-list button").first().click();
  await expect(page.getByRole("heading", { name: "Review workspace" })).toBeVisible();

  const projectUrl = page.url();
  await page.addInitScript(() => {
    class UnavailableWebSocket {
      addEventListener() {}
      close() {}
    }
    Object.defineProperty(window, "WebSocket", { value: UnavailableWebSocket });
  });
  await page.reload();
  await expect(page.getByRole("heading", { name: "Review workspace" })).toBeVisible();
  const second = await context.newPage();
  await second.goto(projectUrl);
  await expect(second.getByRole("heading", { name: "Review workspace" })).toBeVisible();

  const firstNote = page.getByLabel("Revision note");
  const secondNote = second.getByLabel("Revision note");
  await firstNote.fill("Concurrent note from tab one");
  await secondNote.fill("Concurrent note from tab two");
  await Promise.all([
    page.getByRole("button", { name: "Save note" }).click(),
    second.getByRole("button", { name: "Save note" }).click(),
  ]);

  const projectId = decodeURIComponent(new URL(projectUrl).pathname.split("/").at(-1)!);
  await expect.poll(async () => page.evaluate(async ({ id }) => {
    const response = await fetch(`/api/v1/projects/${encodeURIComponent(id)}/assets/asset_web_e2e/review`);
    return response.json();
  }, { id: projectId }), { timeout: 12_000 }).toMatchObject({
    noteRevisionCount: 2,
    activeNote: expect.stringMatching(/Concurrent note from tab (one|two)/),
  });

  await second.getByLabel("Add label").fill("polling-recovery");
  await second.getByRole("button", { name: "Add", exact: true }).click();
  await expect(page.getByRole("button", { name: "Remove label polling-recovery" })).toBeVisible({ timeout: 12_000 });
  await expect.poll(async () => {
    const values = await Promise.all([firstNote.inputValue(), secondNote.inputValue()]);
    return values[0] === values[1] ? values[0] : "not-converged";
  }, { timeout: 12_000 }).toMatch(/Concurrent note from tab (one|two)/);
});
