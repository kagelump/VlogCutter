/**
 * The failure matrix.
 *
 * These are the conditions that are expensive or impossible to stage against
 * real hardware — a locked database, a volume pulled mid-session, a backend
 * that will not answer — and they are consequently the ones almost never
 * tested. Every case here asserts the same three things:
 *
 *   1. the user is told what failed;
 *   2. the work in progress survives;
 *   3. the same action succeeds once the fault clears.
 *
 * "It did not crash" is not an acceptance.
 */

import { screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, queueFilenames, renderApp, selectedRow, type GenerationOutcome, type GenerationStep } from "../harness";

function session() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 4, withProxy: 2, startAt: "2026-02-08T00:10:00.000Z" })
    .build();
}

const first = "asset_sony_a7c_ii_001";

describe("a save that fails", () => {
  it("keeps the draft on screen, marks it unsaved, and retries it unchanged", async () => {
    const harness = session();
    const app = await renderApp(harness);
    harness.next("update_asset").reject("database is locked");

    await app.user.type(screen.getByLabelText("Notes"), "The light turns about here");
    await app.user.click(screen.getByText("SONY_002.MP4"));

    // Told what failed, and nothing was written.
    expect(await screen.findByText(/database is locked/)).toBeVisible();
    expect(harness.state.assets[first].notes).toBe("");

    // The draft survives, and says so. The failure policy for human prose is
    // retain-draft: the paragraph stays, marked as not in the catalog rather
    // than sitting there looking saved.
    await app.user.click(screen.getByText("SONY_001.MP4"));
    await waitFor(() => expect(screen.getByLabelText("Notes")).toHaveValue("The light turns about here"));
    expect(screen.getByText("Unsaved")).toBeVisible();

    // The same draft is sent again, with no edit to retrigger a blur.
    await app.user.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(harness.state.assets[first].notes).toBe("The light turns about here"));
    expect(screen.queryByText("Unsaved")).toBeNull();
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
  });

  it("does not claim a draft is saved while the backend still holds the old note", async () => {
    const harness = session();
    const app = await renderApp(harness);
    harness.always("update_asset").reject("database is locked");

    await app.user.type(screen.getByLabelText("Notes"), "Snow starts here");
    await app.user.click(screen.getByText("SONY_002.MP4"));
    await screen.findByText(/database is locked/);

    // Away and back inside the same renderer session: still the draft, still
    // marked. Nothing in the interface says "Saved".
    await app.user.click(screen.getByText("SONY_003.MP4"));
    await app.user.click(screen.getByText("SONY_001.MP4"));
    await waitFor(() => expect(screen.getByLabelText("Notes")).toHaveValue("Snow starts here"));
    expect(screen.getByText("Unsaved")).toBeVisible();
    expect(screen.queryByText("Saved")).toBeNull();
    expect(harness.state.assets[first].notes).toBe("");

    // A reload answers with what the catalog holds, which is not the draft —
    // and the draft is still the thing on screen.
    await app.user.click(screen.getByLabelText("Reload clips"));
    await waitFor(() => expect(harness.calls("list_assets").length).toBeGreaterThan(1));
    expect(screen.getByLabelText("Notes")).toHaveValue("Snow starts here");
    expect(screen.getByText("Unsaved")).toBeVisible();
  });

  it("does not leave a label showing that the backend refused", async () => {
    const harness = session();
    const app = await renderApp(harness);
    harness.next("update_asset").reject("database is locked");

    await app.user.type(screen.getByLabelText("Add label"), "arrival{Enter}");

    expect(await screen.findByText(/database is locked/)).toBeVisible();
    // The optimistic chip is rolled back rather than left as a lie.
    await waitFor(() => expect(screen.queryByLabelText("Remove arrival")).toBeNull());
    expect(harness.state.assets[first].labels).toEqual([]);

    await app.user.type(screen.getByLabelText("Add label"), "arrival{Enter}");
    await waitFor(() => expect(harness.state.assets[first].labels).toEqual(["arrival"]));
  });
});

describe("a project that will not open", () => {
  it("says the catalog is unavailable and recovers on the retry it offers", async () => {
    const harness = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 3, withProxy: true })
      .build();
    harness.always("list_assets").reject("catalog unavailable: database is locked");

    const app = await renderApp(harness);

    expect(await screen.findAllByText(/catalog unavailable/)).not.toHaveLength(0);
    expect(queueFilenames()).toEqual([]);

    // The failure leaves a way back in: the project's reads have an owner, and
    // the empty queue offers to run them again. Relaunching the app is no
    // longer the only recovery from a locked catalog.
    harness.clearFaults();
    await app.user.click(screen.getByRole("button", { name: "Try again" }));

    await waitFor(() => expect(queueFilenames()).toHaveLength(3));
    expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
  });

  it("keeps offering the retry until it succeeds", async () => {
    const harness = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 3, withProxy: true })
      .build();
    harness.always("list_assets").reject("catalog unavailable: database is locked");

    const app = await renderApp(harness);
    await screen.findByRole("button", { name: "Try again" });

    // The fault has not cleared, so the retry fails and the offer stands.
    await app.user.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(harness.calls("list_assets").length).toBeGreaterThan(1));
    expect(screen.getByRole("button", { name: "Try again" })).toBeVisible();
    expect(queueFilenames()).toEqual([]);

    harness.clearFaults();
    await app.user.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(queueFilenames()).toHaveLength(3));
  });
});

describe("a generation batch that dies partway", () => {
  it("keeps what it finished and can be run again for the rest", async () => {
    const harness = session();
    const app = await renderApp(harness);
    const job = harness.job<GenerationStep, GenerationOutcome>("generate_missing_proxies");

    await app.user.click(screen.getByRole("button", { name: /Make 2 proxies/ }));
    await job.started();
    await job.progress({ assetId: "asset_sony_a7c_ii_003" });
    await job.fail("The source volume disconnected");

    expect(await screen.findByText(/source volume disconnected/)).toBeVisible();
    expect(harness.state.assets.asset_sony_a7c_ii_003.proxyAvailable).toBe(true);
    expect(harness.state.assets.asset_sony_a7c_ii_004.proxyAvailable).toBe(false);

    // The second run has one clip left to do, and does it.
    await app.user.click(await screen.findByRole("button", { name: /Make 1 proxies/ }));
    await waitFor(() => expect(harness.state.assets.asset_sony_a7c_ii_004.proxyAvailable).toBe(true));
  });
});

describe("a command that does not come back", () => {
  it("shows the operation as running and refuses a second submit until it does", async () => {
    const harness = session();
    const app = await renderApp(harness);
    harness.command("generate_missing_proxies").delayUntilReleased();

    await app.user.click(screen.getByRole("button", { name: /Make 2 proxies/ }));

    // Busy: the button is disabled rather than able to start a second batch.
    await waitFor(() => expect(screen.getByRole("button", { name: /Make 2 proxies/ })).toBeDisabled());
    expect(screen.getByRole("progressbar")).toBeVisible();
    expect(harness.calls("generate_missing_proxies")).toHaveLength(1);

    harness.command("generate_missing_proxies").clear();
    harness.release("generate_missing_proxies");

    await waitFor(() => expect(screen.getByText(/Made 2 proxies/)).toBeVisible());
    expect(harness.calls("generate_missing_proxies")).toHaveLength(1);
  });
});

describe("the source volume disappears mid-session", () => {
  it("keeps the selection and the queue while media goes dark", async () => {
    const harness = session();
    const app = await renderApp(harness);

    await app.user.click(screen.getByText("SONY_003.MP4"));
    harness.asset("asset_sony_a7c_ii_003").setOriginalAvailable(false);
    // A read, asked for as a read: the backend changed underneath the session
    // and the queue is told to look again. No write is needed to notice it.
    await app.user.click(screen.getByLabelText("Reload clips"));

    await waitFor(() => expect(harness.state.assets.asset_sony_a7c_ii_003.originalAvailable).toBe(false));
    expect(selectedRow()?.textContent).toContain("SONY_003.MP4");
    expect(queueFilenames()).toHaveLength(4);
    expect(await screen.findByText("No review proxy yet")).toBeVisible();
  });
});

describe("the media element fails", () => {
  it("keeps the selection and the draft while the app checks its origin", async () => {
    const harness = session();
    const app = await renderApp(harness);

    await app.user.type(screen.getByLabelText("Notes"), "half a thought");
    harness.next("recover_media_origin").reject("the media server is not answering");

    await harness.media(first).failed("origin");

    expect(await screen.findByText(/media server is not answering/)).toBeVisible();
    expect(screen.getByLabelText("Notes")).toHaveValue("half a thought");
    expect(selectedRow()?.textContent).toContain("SONY_001.MP4");
  });
});
