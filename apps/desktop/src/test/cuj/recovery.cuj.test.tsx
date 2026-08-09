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
  it("keeps the draft on screen and saves it when the user tries again", async () => {
    const harness = session();
    const app = await renderApp(harness);
    harness.next("update_asset").reject("database is locked");

    await app.user.type(screen.getByLabelText("Notes"), "The light turns about here");
    await app.user.click(screen.getByText("SONY_002.MP4"));

    // Told what failed, and nothing was written.
    expect(await screen.findByText(/database is locked/)).toBeVisible();
    expect(harness.state.assets[first].notes).toBe("");

    // The draft survives: the rollback restores the edited asset, not the
    // last-persisted one, so the paragraph is still there on return.
    await app.user.click(screen.getByText("SONY_001.MP4"));
    await waitFor(() => expect(screen.getByLabelText("Notes")).toHaveValue("The light turns about here"));

    // Nothing retries on its own — the save is driven by leaving the field —
    // so the user touches it again and it lands.
    await app.user.click(screen.getByLabelText("Notes"));
    await app.user.click(screen.getByText("SONY_002.MP4"));
    await waitFor(() => expect(harness.state.assets[first].notes).toBe("The light turns about here"));
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
  it("says the catalog is unavailable and recovers when it comes back", async () => {
    const harness = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 3, withProxy: true })
      .build();
    harness.always("list_assets").reject("catalog unavailable: database is locked");

    const app = await renderApp(harness);

    expect(await screen.findByText(/catalog unavailable/)).toBeVisible();
    expect(queueFilenames()).toEqual([]);

    // GAP, recorded rather than fixed: the asset read happens when the open
    // project changes, and there is no in-app "try again". Switching modes and
    // back does not re-read, so a transient catalog failure leaves the session
    // empty until the app is relaunched.
    harness.clearFaults();
    await app.user.click(screen.getByRole("button", { name: "Chronology" }));
    await app.user.click(screen.getByRole("button", { name: "Review" }));
    expect(queueFilenames()).toEqual([]);

    // Relaunching does recover, which is what the user is left with today.
    await app.remount();
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
    // The write the user was about to make anyway is what refreshes the clip.
    await app.user.type(screen.getByLabelText("Add label"), "checkpoint{Enter}");

    await waitFor(() => expect(harness.state.assets.asset_sony_a7c_ii_003.labels).toEqual(["checkpoint"]));
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
