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
  await expect(page.getByRole("heading", { name: "Review targets" })).toBeVisible();
  // The fixture seeds one voice-reviewed target with a different YouTube link.
  await expect(page.locator(".tv-review-columns .project-list button")).toHaveCount(1);

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
  await expect(page.locator(".project-list button")).toHaveCount(3);
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
  // Includes real-time capture, authenticated requests, recovery and deletion.
  test.setTimeout(60_000);
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

  // Transcription is explicit and honest about a server without models.
  await page.getByRole("button", { name: "Check" }).click();
  await expect(page.getByText(/speech models are not installed on this server/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Transcribe" })).toHaveCount(0);
  await page.getByLabel(/OpenRouter cloud/).check();
  await page.getByRole("button", { name: "Check" }).click();
  await expect(page.getByText(/may be sent to the selected provider/)).toBeVisible();
  await expect(page.getByText(/No OpenRouter credential/)).toBeVisible();

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
  await page.getByRole("button", { name: "Back to target" }).click();
  await page.locator(".tv-voice-start .project-list button").first().click();
  await expect(page.getByText("Completed", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Delete recording…" }).click();
  await page.getByRole("button", { name: "Delete audio permanently" }).click();
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
  await expect(page.locator(".tv-recorder-facts")).toContainText(/Cues\s*1/);
  await page.waitForTimeout(500);
  await page.reload();
  await page.goto(targetUrl);
  const recovery = page.getByRole("region", { name: "Unfinished recordings" });
  await expect(recovery).toBeVisible();
  await page.unroute("**/review-sessions/*/events");
  await recovery.getByRole("button", { name: "Resume upload and finalize" }).click();
  await expect(page.getByText("Buffered recordings uploaded and finalized.")).toBeVisible({ timeout: 30_000 });
  await page.reload();
  await page.locator(".tv-voice-start .project-list button").first().click();
  await expect(page.getByRole("heading", { name: "Voice review session" })).toBeVisible();
  await expect(page.getByText(/^interrupted/)).toBeVisible();
  await expect(page.getByText("Incomplete",{exact:true})).toBeVisible();
  await expect(page.getByText(/reviewer declaration, not measured sync/)).toBeVisible();
});


test("Chrome turns a two-reviewer transcript into grouped discussions with a retraction and one human-accepted edit", async ({ page }) => {
  const baseURL = process.env.RCDVLOG_WEB_E2E_BASE_URL;
  if (!baseURL) throw new Error("web e2e: no RCDVLOG_WEB_E2E_BASE_URL");
  const reviewPath = readFileSync(reviewRunPath(baseURL), "utf8").trim();
  await page.goto("/");
  await page.getByLabel("Bootstrap code").fill(bootstrapCode(7));
  await page.getByRole("button", { name: "Open review desk" }).click();
  await expect(page.getByRole("heading", { name: "Review desk ready" })).toBeVisible();
  await page.goto(reviewPath);
  await expect(page.getByRole("heading", { name: "Editorial review" })).toBeVisible();
  await expect(page.getByText("Movie time: shared sync")).toBeVisible();
  // Raw Simplified text is preserved; Traditional is the display projection.
  await expect(page.getByText("不要，長度不變，換個轉場。")).toBeVisible();
  await expect(page.getByText(/raw: 不要，长度不变，换个转场。/)).toBeVisible();

  const ids = await page.locator("select[aria-label^='Speaker for']").evaluateAll((nodes) => nodes.map((n) => n.getAttribute("aria-label")!));
  await page.getByLabel(ids[0]).selectOption("A");
  await page.getByLabel(ids[1]).selectOption("B");
  await expect(page.getByLabel(ids[1])).toHaveValue("B");

  await page.getByRole("button", { name: "Group into discussions" }).click();
  const card = page.locator(".tv-discussion").first();
  await expect(card).toBeVisible();

  async function add(kind: string, speaker: string, text: string, refers?: string, decision?: string) {
    await card.getByLabel("Item kind").selectOption(kind);
    await card.getByLabel("Said by").selectOption(speaker);
    if (refers) await card.getByLabel("Refers to").selectOption({ label: refers });
    if (decision) await card.getByLabel("Decision").selectOption(decision);
    await card.getByLabel("Item text").fill(text);
    await card.getByRole("button", { name: "Add" }).click();
  }
  await add("candidateEdit", "A", "Keep this shot longer.");
  await expect(card.getByText("Speaker A: Keep this shot longer.")).toBeVisible();
  await add("retraction", "B", "No, keep the duration; change the transition.", "Candidate edit: Keep this shot longer.");
  await add("candidateEdit", "B", "Change the transition, keep the duration.");
  await expect(card.locator(".tv-item-retracted")).toHaveCount(1);

  await add("decision", "unknown", "accept the retracted one", "Candidate edit: Keep this shot longer.", "accepted");
  await expect(page.getByText(/was retracted in the conversation/)).toBeVisible();
  await add("decision", "unknown", "accept transition", "Candidate edit: Change the transition, keep the duration", "accepted");
  await expect(page.getByText(/Confirm which cut this discussion is about/)).toBeVisible();
  await card.getByLabel("Target cut").selectOption("editop_split_a");
  await add("decision", "unknown", "We agree on the transition.", "Candidate edit: Change the transition, keep the duration", "accepted");
  await expect(card.locator(".tv-item-accepted")).toHaveCount(1);

  const href = await page.getByRole("link", { name: "Agent report (JSON)" }).getAttribute("href");
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
  await page.goto("/");
  await page.getByLabel("Bootstrap code").fill(bootstrapCode(8));
  await page.getByRole("button", { name: "Open review desk" }).click();
  await page.getByRole("link", { name: "TV review" }).click();
  await page.locator(".project-list button").first().click();
  await expect(page.getByRole("heading", { name: "Review targets" })).toBeVisible();

  // The page detects the purpose discriminator and asks for the acknowledgement.
  await page.getByLabel("Handoff JSON").fill(review);
  await expect(page.getByRole("group", { name: "Historical rendered-artifact review" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Check handoff" })).toBeDisabled();
  await page.getByRole("checkbox", { name: /already rendered movie only/ }).check();
  await page.getByRole("button", { name: "Check handoff" }).click();
  await expect(page.getByText("Ready to import as a new immutable historical target.")).toBeVisible();
  await expect(page.getByText("Historical rendered artifact (integrity only)")).toBeVisible();
  await expect(page.locator(".tv-checks li[data-status=warn]").filter({ hasText: "currentRenderability" })).toBeVisible();
  await page.getByRole("button", { name: "Import historical target" }).click();

  await expect(page.getByRole("heading", { name: "Revision editrev_historical" })).toBeVisible();
  await expect(page.getByText("Historical artifact · integrity only")).toBeVisible();
  await expect(page.getByRole("note")).toContainText("nothing here can render, edit, activate or upload it");
  await expect(page.getByText("Current plan renderability was not established by this review", { exact: false })).toBeVisible();
  // Recording is still allowed: discussing an already rendered movie is the point.
  await expect(page.getByRole("button", { name: "Start voice review" })).toBeEnabled();
  await page.getByRole("button", { name: "All targets" }).click();
  await expect(page.locator(".tv-review-columns .project-list button").filter({ hasText: "editrev_historical" })).toContainText("Historical artifact · integrity only");
});
