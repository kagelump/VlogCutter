import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";

import { bootstrapCodePath, historicalReviewPath, reviewRunPath, reviewTargetHandoffPath } from "../../../scripts/lib/web-e2e-fixture.mjs";

function bootstrapCode(index: number): string {
  const configured = process.env.RCDVLOG_E2E_BOOTSTRAP_CODES?.split(",")[index - 1]?.trim();
  if (configured) return configured;
  // Keyed to this run's port, which the config exported after allocating it.
  const baseURL = process.env.RCDVLOG_WEB_E2E_BASE_URL;
  if (!baseURL) throw new Error("web e2e: no RCDVLOG_WEB_E2E_BASE_URL and no RCDVLOG_E2E_BOOTSTRAP_CODES");
  return readFileSync(bootstrapCodePath(baseURL, index), "utf8").trim();
}

async function signIn(page: import("@playwright/test").Page, code: string) {
  await page.goto("/");
  await page.getByLabel("Sign-in code").fill(code);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { name: "Projects" })).toBeVisible();
}

test("Chrome reaches the real unauthenticated server session flow", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();
  await expect(page.getByLabel("Sign-in code")).toBeVisible();
});

test("Chrome can exchange a real one-time code and load the shell", async ({ page }) => {
  await signIn(page, bootstrapCode(1));
  await expect(page.getByRole("navigation", { name: "Main" })).toBeVisible();
  // A ready server shows no health warning in the header.
  await expect(page.getByRole("link", { name: /not found|server issues/ })).toHaveCount(0);
});

test("Chrome administration loads durable state and records a root mutation", async ({ page }) => {
  await page.setViewportSize({ width: 980, height: 680 });
  await signIn(page, bootstrapCode(2));
  await page.getByRole("link", { name: "Admin" }).click();
  await expect(page.getByRole("heading", { name: /^Admin/ })).toBeVisible();
  await expect(page.getByText("All checks pass")).toBeVisible();
  await page.getByRole("button", { name: "Add storage" }).click();
  const dialog = page.getByRole("dialog", { name: "Add storage" });
  await dialog.getByLabel("Name").fill("Chrome archive");
  await dialog.getByRole("button", { name: "Add storage" }).click();
  await expect(page.locator("#storage").getByText("Chrome archive", { exact: true })).toBeVisible();
  await expect(page.getByText("storage_root.created")).toBeVisible();
});

test("two Chrome tabs converge concurrent notes when one WebSocket is unavailable", async ({ page, context }) => {
  await signIn(page, bootstrapCode(3));
  await page.getByRole("link", { name: "Clips" }).first().click();
  await expect(page.getByLabel(/^Note/)).toBeVisible();

  const projectUrl = page.url();
  await page.addInitScript(() => {
    class UnavailableWebSocket {
      addEventListener() {}
      close() {}
    }
    Object.defineProperty(window, "WebSocket", { value: UnavailableWebSocket });
  });
  await page.reload();
  await expect(page.getByLabel(/^Note/)).toBeVisible();
  const second = await context.newPage();
  await second.goto(projectUrl);
  await expect(second.getByLabel(/^Note/)).toBeVisible();

  const firstNote = page.getByLabel(/^Note/);
  const secondNote = second.getByLabel(/^Note/);
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

test("Chrome imports a verified TV review target through the real server and shows its shots", async ({ page }) => {
  const baseURL = process.env.RCDVLOG_WEB_E2E_BASE_URL;
  if (!baseURL) throw new Error("web e2e: no RCDVLOG_WEB_E2E_BASE_URL");
  const handoff = readFileSync(reviewTargetHandoffPath(baseURL), "utf8");
  await page.setViewportSize({ width: 980, height: 680 });
  await signIn(page, bootstrapCode(4));
  await page.getByRole("link", { name: "TV reviews" }).first().click();
  // The fixture seeds one voice-reviewed target with a different YouTube link.
  const targets = page.getByRole("main").getByRole("listitem");
  await expect(targets).toHaveCount(1);

  // A tampered copy is refused by the shared Rust digest before anything is stored.
  const tampered = JSON.parse(handoff);
  tampered.target.cuts[0].sourceInSeconds = 1.25;
  await page.getByRole("button", { name: "Import cut" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Import a cut" });
  await dialog.getByLabel("Export JSON").fill(JSON.stringify(tampered));
  await dialog.getByRole("button", { name: "Check" }).click();
  await expect(dialog.getByText(/edited after export/)).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Import", exact: true })).toHaveCount(0);

  await dialog.getByLabel("Export JSON").fill(handoff);
  await dialog.getByLabel("Exported from").fill("Synthetic Mac");
  await dialog.getByRole("button", { name: "Check" }).click();
  await expect(dialog.getByText("Ready to import.")).toBeVisible();
  await dialog.getByRole("button", { name: "Import", exact: true }).click();
  await expect(page.getByRole("heading", { name: /^Cut syntheti/ })).toBeVisible();
  await expect(page.getByText("Ready", { exact: true })).toBeVisible();
  // The export declared no YouTube video, so the page offers no YouTube link.
  await expect(page.getByRole("link", { name: "YouTube" })).toHaveCount(0);
  await expect(page.getByRole("region", { name: /Shots/ }).getByRole("link")).toHaveCount(3);

  // Re-importing the identical handoff reuses the original target rather than adding one.
  await page.getByRole("link", { name: "TV reviews" }).first().click();
  await expect(targets).toHaveCount(2);
  await page.getByRole("button", { name: "Import cut" }).first().click();
  await dialog.getByLabel("Export JSON").fill(handoff);
  await dialog.getByRole("button", { name: "Check" }).click();
  await expect(dialog.getByText("Already imported.")).toBeVisible();
});

async function openSeededTarget(page: import("@playwright/test").Page, code: string) {
  const baseURL = process.env.RCDVLOG_WEB_E2E_BASE_URL;
  if (!baseURL) throw new Error("web e2e: no RCDVLOG_WEB_E2E_BASE_URL");
  const handoff = readFileSync(reviewTargetHandoffPath(baseURL), "utf8");
  await signIn(page, code);
  await page.getByRole("link", { name: "TV reviews" }).first().click();
  await page.getByRole("button", { name: "Import cut" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Import a cut" });
  await dialog.getByLabel("Export JSON").fill(handoff);
  await dialog.getByRole("button", { name: "Check" }).click();
  await dialog.getByRole("button", { name: /^(Import|Open existing)$/ }).click();
  await expect(page.getByRole("heading", { name: /^Cut syntheti/ })).toBeVisible();
}

async function startRecording(page: import("@playwright/test").Page) {
  await page.getByRole("link", { name: "Record review" }).click();
  await expect(page.getByRole("heading", { name: "Record review" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Record", exact: true })).toBeDisabled();
  await page.getByRole("button", { name: "Allow microphone" }).click();
  await expect(page.getByLabel("Input", { exact: true })).toBeVisible();
  await page.getByLabel(/Everyone here knows/).check();
  await page.getByRole("button", { name: "Record", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: /^Recording/ })).toBeVisible();
}

test("Chrome records a fake-microphone voice review with a manual cue and reopens it", async ({ page }) => {
  // Includes real-time capture, authenticated requests, recovery and deletion.
  test.setTimeout(60_000);
  await openSeededTarget(page, bootstrapCode(5));
  await startRecording(page);
  await page.waitForTimeout(1_500);
  await page.keyboard.press("m");
  await page.waitForTimeout(3_000);
  await expect(page.getByLabel("Recorder")).toContainText(/Uploaded\s*[1-9]/);
  await page.getByRole("button", { name: "Stop" }).click();
  await expect(page.getByText("Recorded TV review")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByText("Completed", { exact: true })).toBeVisible();
  await page.getByText("Recording details").click();
  await expect(page.getByText(/your mark, not measured sync/)).toBeVisible();
  await expect(page.getByText(/echo cancellation off/)).toBeVisible();
  // Sample-clock / performance-clock pairs were queued, posted and kept.
  await expect(page.getByText(/sample\/performance clock pairs across 1 clock epoch$/)).toBeVisible();

  // Transcription is explicit and honest about a server without models.
  await page.getByRole("button", { name: "Check" }).click();
  await expect(page.getByText(/Speech models aren't installed on this server/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Transcribe" })).toHaveCount(0);
  await page.getByRole("button", { name: "OpenRouter (cloud)" }).click();
  await page.getByRole("button", { name: "Check" }).click();
  await expect(page.getByText(/may be sent to the provider/)).toBeVisible();
  // A developer may have a key file even when the fixture clears the env key.
  // Capture must never enable cloud execution without explicit consent.
  await expect(page.getByRole("checkbox", { name: /Send this recording to OpenRouter/ })).not.toBeChecked();
  await expect(page.getByRole("button", { name: "Transcribe", exact: true })).toHaveCount(0);

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
  const sessionApi=href!.replace(/\/audio\.wav$/,"");
  const evidence=await page.evaluate(async(url)=>{const response=await fetch(url);return response.json();},sessionApi);
  const detail=evidence;
  // The recorder also keeps getOutputTimestamp pairs; the paired native-frame
  // probe evidence is selected by its schema, not by arrival order.
  const calibrations=detail.events.filter((e:{kind:string;detail?:{schema?:string}})=>e.kind==="clockSample"&&e.detail?.schema==="review-capture-calibration.v1");
  expect(calibrations.length).toBeGreaterThan(0);
  expect(calibrations[0].detail).toMatchObject({schema:"review-capture-calibration.v1",method:"workletProbeBracket",clockId:"mic_epoch_0",sampleRate:48000,firstSample:0});
  expect(calibrations[0].detail.monotonicToleranceNs).toBeLessThan(1_000_000_000);
  expect(calibrations[0].detail.probeSentNs).toBeLessThanOrEqual(calibrations[0].detail.probeReceivedNs);
  expect(detail.coverage.finalFrameCount*detail.channels*2).toBe(wav.length-44);

  // Native 48 kHz PCM16: more than four seconds of audio, preserving channels.
  expect(wav.length - 44).toBeGreaterThan(4 * 48_000 * 2 * 0.9);

  // Reopen from the target's session list.
  await page.getByRole("link", { name: "Cut", exact: true }).click();
  await page.getByRole("region", { name: "Recordings" }).getByRole("link").first().click();
  await expect(page.getByText("Completed", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Delete audio" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Delete audio" }).click();
  await expect(page.getByText(/Audio deleted/)).toBeVisible();
  expect((await download(href!)).status).toBe(409);
});

test("Chrome recovers a recording interrupted by a page reload and finalizes it honestly", async ({ page }) => {
  // Includes real-time capture, authenticated requests, recovery and deletion.
  test.setTimeout(60_000);
  await openSeededTarget(page, bootstrapCode(6));
  const targetUrl = page.url();
  await startRecording(page);
  await page.waitForTimeout(2_500);
  // Audio remains online while event posts fail. The saved cue must survive
  // page replacement in the real IndexedDB outbox before recovery retries it.
  await page.route("**/review-sessions/*/events",route=>route.abort("failed"));
  await page.keyboard.press("m");
  await expect(page.getByLabel("Recorder")).toContainText(/Start marks\s*1/);
  await page.waitForTimeout(500);
  await page.reload();
  await page.goto(targetUrl);
  await expect(page.getByText(/hasn’t finished uploading/)).toBeVisible();
  await page.unroute("**/review-sessions/*/events");
  await page.getByRole("button", { name: "Finish upload" }).click();
  await expect(page.getByText("Upload finished.")).toBeVisible({ timeout: 30_000 });
  await page.reload();
  await page.getByRole("region", { name: "Recordings" }).getByRole("link").first().click();
  await expect(page.getByText("Recorded TV review")).toBeVisible();
  await expect(page.getByText("Interrupted", { exact: true })).toBeVisible();
  await expect(page.getByText("Incomplete", { exact: true })).toBeVisible();
  await page.getByText("Recording details").click();
  await expect(page.getByText(/your mark, not measured sync/)).toBeVisible();
});


test("Chrome turns a two-reviewer transcript into grouped discussions with a retraction and one human-accepted edit", async ({ page }) => {
  const baseURL = process.env.RCDVLOG_WEB_E2E_BASE_URL;
  if (!baseURL) throw new Error("web e2e: no RCDVLOG_WEB_E2E_BASE_URL");
  const reviewPath = readFileSync(reviewRunPath(baseURL), "utf8").trim();
  await page.setViewportSize({ width: 1280, height: 900 });
  await signIn(page, bootstrapCode(7));
  // Pre-redesign addresses still resolve.
  await page.goto(reviewPath);
  await expect(page.getByRole("heading", { name: /Review notes/ })).toBeVisible();
  await expect(page.getByText("Timing matched")).toBeVisible();
  // Raw Simplified text is preserved; Traditional is the display projection.
  await expect(page.getByText("不要，長度不變，換個轉場。")).toBeVisible();
  await expect(page.getByText(/original: 不要，长度不变，换个转场。/)).toBeVisible();

  const ids = await page.locator("select[aria-label^='Speaker for']").evaluateAll((nodes) => nodes.map((n) => n.getAttribute("aria-label")!));
  await page.getByLabel(ids[0]).selectOption("A");
  await page.getByLabel(ids[1]).selectOption("B");
  await expect(page.getByLabel(ids[1])).toHaveValue("B");

  await page.getByRole("button", { name: "Group transcript into discussions" }).click();
  const card = page.getByRole("article").first();
  await expect(card).toBeVisible();

  async function add(kind: string, speaker: string, text: string, refers?: string, decision?: string) {
    await card.getByLabel("Type", { exact: true }).selectOption(kind);
    await card.getByLabel("Said by").selectOption(speaker);
    if (refers) await card.getByLabel("Responding to").selectOption({ label: refers });
    if (decision) await card.getByLabel("Decision").selectOption(decision);
    await card.getByLabel("Text", { exact: true }).fill(text);
    await card.getByRole("button", { name: "Add" }).click();
  }
  await add("candidateEdit", "A", "Keep this shot longer.");
  await expect(card.locator("li[data-status]", { hasText: "Speaker A: Keep this shot longer." })).toBeVisible();
  await add("retraction", "B", "No, keep the duration; change the transition.", "Suggested edit: Keep this shot longer.");
  await add("candidateEdit", "B", "Change the transition, keep the duration.");
  await expect(card.locator("li[data-status=retracted]")).toHaveCount(1);

  await add("decision", "unknown", "accept the retracted one", "Suggested edit: Keep this shot longer.", "accepted");
  await expect(page.getByText(/was retracted and can't be accepted/)).toBeVisible();
  await add("decision", "unknown", "accept transition", "Suggested edit: Change the transition, keep the duration", "accepted");
  await expect(page.getByText(/Pick the shot this discussion is about/)).toBeVisible();
  await card.getByLabel("Shot this is about").selectOption("editop_split_a");
  await add("decision", "unknown", "We agree on the transition.", "Suggested edit: Change the transition, keep the duration", "accepted");
  await expect(card.locator("li[data-status=accepted]")).toHaveCount(1);

  const href = await page.getByRole("link", { name: "Agent report" }).getAttribute("href");
  const report = await page.evaluate(async (url) => (await fetch(url!)).json(), href);
  expect(report.schema).toBe("review-report.v1");
  const accepted = report.feedback.filter((f: { humanAccepted: boolean }) => f.humanAccepted);
  expect(accepted).toHaveLength(1);
  expect(accepted[0].record.operationId).toBe("editop_split_a");
  expect(accepted[0].record.author).toBe("human");
  expect(report.feedback.some((f: { record: { observation: string } }) => f.record.observation === "Keep this shot longer.")).toBe(false);
  expect(report.coverage.audiovisualReviewAttested).toBe(false);
});

test("Chrome imports a historical rendered-artifact review only with an acknowledgement and keeps its weaker guarantees", async ({ page }) => {
  const baseURL = process.env.RCDVLOG_WEB_E2E_BASE_URL;
  if (!baseURL) throw new Error("web e2e: no RCDVLOG_WEB_E2E_BASE_URL");
  const review = readFileSync(historicalReviewPath(baseURL), "utf8");
  await signIn(page, bootstrapCode(8));
  await page.getByRole("link", { name: "TV reviews" }).first().click();

  // The page detects the purpose discriminator and asks for the acknowledgement.
  await page.getByRole("button", { name: "Import cut" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Import a cut" });
  await dialog.getByLabel("Export JSON").fill(review);
  await expect(dialog.getByText("Historical review.")).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Check" })).toBeDisabled();
  await dialog.getByRole("checkbox", { name: /covers the rendered movie only/ }).check();
  await dialog.getByRole("button", { name: "Check" }).click();
  await expect(dialog.getByText("Ready to import.")).toBeVisible();
  await expect(dialog.getByText("Historical (integrity only)")).toBeVisible();
  await expect(dialog.locator("li[data-status=warn]").filter({ hasText: "currentRenderability" })).toBeVisible();
  await dialog.getByRole("button", { name: "Import", exact: true }).click();

  await expect(page.getByRole("heading", { name: /^Cut historic/ })).toBeVisible();
  await expect(page.getByText("Historical", { exact: true })).toBeVisible();
  await expect(page.getByRole("note").filter({ hasText: "Historical cut." })).toContainText("nothing here can render, edit, activate or upload it");
  // Recording is still allowed: discussing an already rendered movie is the point.
  await expect(page.getByRole("link", { name: "Record review" })).toBeVisible();
  await page.getByRole("link", { name: "TV reviews" }).first().click();
  await expect(page.getByRole("main").getByRole("listitem").filter({ hasText: "Cut historic" })).toContainText("Historical");
});
