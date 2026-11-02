import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";

import { bootstrapCodePath, reviewTargetHandoffPath } from "../../../scripts/lib/web-e2e-fixture.mjs";

function bootstrapCode(index: number): string {
  const configured = process.env.RCDVLOG_E2E_BOOTSTRAP_CODES?.split(",")[index - 1]?.trim();
  if (configured) return configured;
  // Keyed to this run's port, which the config exported after allocating it.
  const baseURL = process.env.RCDVLOG_WEB_E2E_BASE_URL;
  if (!baseURL) throw new Error("web e2e: no RCDVLOG_WEB_E2E_BASE_URL and no RCDVLOG_E2E_BOOTSTRAP_CODES");
  return readFileSync(bootstrapCodePath(baseURL, index), "utf8").trim();
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
    // Three, not two: `reconcile_asset_baseline` writes a baseline note
    // revision the first time an asset is touched by an operation, so a clip
    // that has been saved twice carries that plus one per save. The server's
    // own assertions use `>= 2` for the same reason. Exact here because this
    // suite controls both writers, which makes a fourth revision a real
    // double-write in the polling-recovery path rather than noise.
    noteRevisionCount: 3,
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

test("Chrome imports a verified TV review target through the real server and declares a YouTube link", async ({ page }) => {
  const baseURL = process.env.RCDVLOG_WEB_E2E_BASE_URL;
  if (!baseURL) throw new Error("web e2e: no RCDVLOG_WEB_E2E_BASE_URL");
  const handoff = readFileSync(reviewTargetHandoffPath(baseURL), "utf8");
  await page.setViewportSize({ width: 980, height: 680 });
  await page.goto("/");
  await page.getByLabel("Bootstrap code").fill(bootstrapCode(4));
  await page.getByRole("button", { name: "Open review desk" }).click();
  await page.getByRole("link", { name: "TV review" }).click();
  await expect(page.getByRole("heading", { name: "TV review targets" })).toBeVisible();
  await page.locator(".project-list button").first().click();
  await expect(page.getByText("No review targets yet")).toBeVisible();

  // A tampered copy is refused by the shared Rust digest before anything is stored.
  const tampered = JSON.parse(handoff);
  tampered.target.cuts[0].sourceInSeconds = 1.25;
  await page.getByLabel("Handoff JSON").fill(JSON.stringify(tampered));
  await page.getByRole("button", { name: "Check handoff" }).click();
  await expect(page.getByText(/modified after export/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Import target" })).toHaveCount(0);

  await page.getByLabel("Handoff JSON").fill(handoff);
  await page.getByLabel("Owning host label (optional)").fill("Synthetic Mac");
  await page.getByRole("button", { name: "Check handoff" }).click();
  await expect(page.getByText("Ready to import as a new immutable target.")).toBeVisible();
  await page.getByRole("button", { name: "Import target" }).click();
  await expect(page.getByRole("heading", { name: "Revision editrev_synthetic" })).toBeVisible();
  await expect(page.getByText(/Verified measured mapping/)).toBeVisible();
  await expect(page.getByText("Ready for a session")).toBeVisible();
  await expect(page.getByText("No YouTube video declared")).toBeVisible();
  await expect(page.locator(".tv-cuts tbody tr")).toHaveCount(3);

  await page.getByLabel("YouTube link or video ID").fill("https://youtu.be/synthetic02?si=share");
  await page.getByRole("button", { name: "Declare video" }).click();
  await expect(page.getByRole("heading", { name: "Video synthetic02" })).toBeVisible();
  await expect(page.getByText(/Unverified: nothing proves YouTube/)).toBeVisible();
  await expect(page.getByRole("link", { name: /earlier target without this YouTube link/ })).toBeVisible();

  // Re-importing the identical handoff reuses the original target rather than adding one.
  await page.getByRole("button", { name: "All targets" }).click();
  await expect(page.locator(".project-list button")).toHaveCount(2);
  await page.getByLabel("Handoff JSON").fill(handoff);
  await page.getByRole("button", { name: "Check handoff" }).click();
  await expect(page.getByText("Identical target already imported.")).toBeVisible();
});

async function openSeededTarget(page: import("@playwright/test").Page, code: string) {
  const baseURL = process.env.RCDVLOG_WEB_E2E_BASE_URL;
  if (!baseURL) throw new Error("web e2e: no RCDVLOG_WEB_E2E_BASE_URL");
  const handoff = readFileSync(reviewTargetHandoffPath(baseURL), "utf8");
  await page.goto("/");
  await page.getByLabel("Bootstrap code").fill(code);
  await page.getByRole("button", { name: "Open review desk" }).click();
  await page.getByRole("link", { name: "TV review" }).click();
  await page.locator(".project-list button").first().click();
  await page.getByLabel("Handoff JSON").fill(handoff);
  await page.getByRole("button", { name: "Check handoff" }).click();
  await page.getByRole("button", { name: /Import target|Open existing target/ }).click();
  await expect(page.getByRole("heading", { name: "Revision editrev_synthetic" })).toBeVisible();
}

async function startRecording(page: import("@playwright/test").Page) {
  await page.getByRole("button", { name: "Start voice review" }).click();
  await expect(page.getByRole("heading", { name: "Record a TV review" })).toBeVisible();
  await expect(page.getByRole("button", { name: "● Record" })).toBeDisabled();
  await page.getByRole("button", { name: "Allow microphone" }).click();
  await expect(page.getByLabel("Input device")).toBeVisible();
  await page.getByLabel(/Everyone present knows/).check();
  await page.getByRole("button", { name: "● Record" }).click();
  await expect(page.locator(".tv-recording-indicator")).toBeVisible();
}

test("Chrome records a fake-microphone voice review with a manual cue and reopens it", async ({ page }) => {
  await openSeededTarget(page, bootstrapCode(5));
  await startRecording(page);
  await page.waitForTimeout(1_500);
  await page.keyboard.press("m");
  await page.waitForTimeout(3_000);
  await expect(page.locator(".tv-recorder-facts")).toContainText(/Uploaded\s*[1-9]/);
  await page.getByRole("button", { name: "■ Stop" }).click();
  await expect(page.getByRole("heading", { name: "Voice review session" })).toBeVisible({ timeout: 30_000 });
  await expect(page.getByText("Completed", { exact: true })).toBeVisible();
  await expect(page.getByText(/reviewer declaration, not measured sync/)).toBeVisible();
  await expect(page.getByText(/echo cancellation false/)).toBeVisible();
  // Sample-clock / performance-clock pairs were queued, posted and kept.
  await expect(page.getByText(/sample\/performance clock pairs across 1 clock epoch$/)).toBeVisible();

  const href = await page.getByRole("link", { name: "Download WAV" }).getAttribute("href");
  // Fetch from the page so the browser's own session cookie is used.
  const download = (url: string) => page.evaluate(async (target) => {
    const response = await fetch(target);
    const bytes = new Uint8Array(await response.arrayBuffer());
    return { status: response.status, length: bytes.length, magic: String.fromCharCode(...bytes.subarray(0, 4)) };
  }, url);
  const wav = await download(href!);
  expect(wav.status).toBe(200);
  expect(wav.magic).toBe("RIFF");
  // 48 kHz mono PCM16: more than four seconds of audio.
  expect(wav.length - 44).toBeGreaterThan(4 * 48_000 * 2 * 0.9);

  // Reopen from the target's session list.
  await page.getByRole("button", { name: "Back to target" }).click();
  await page.locator(".tv-voice-start .project-list button").first().click();
  await expect(page.getByText("Completed", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Delete recording…" }).click();
  await page.getByRole("button", { name: "Delete audio permanently" }).click();
  await expect(page.getByText(/Audio deleted/)).toBeVisible();
  expect((await download(href!)).status).toBe(409);
});

test("Chrome recovers a recording interrupted by a page reload and finalizes it honestly", async ({ page }) => {
  await openSeededTarget(page, bootstrapCode(6));
  const targetUrl = page.url();
  await startRecording(page);
  await page.waitForTimeout(4_500);
  await page.reload();
  await page.goto(targetUrl);
  const recovery = page.getByRole("region", { name: "Unfinished recordings" });
  await expect(recovery).toBeVisible();
  await recovery.getByRole("button", { name: "Resume upload and finalize" }).click();
  await expect(page.getByText("Buffered recordings uploaded and finalized.")).toBeVisible({ timeout: 30_000 });
  await page.reload();
  await page.locator(".tv-voice-start .project-list button").first().click();
  await expect(page.getByRole("heading", { name: "Voice review session" })).toBeVisible();
  await expect(page.getByText(/^interrupted/)).toBeVisible();
});
