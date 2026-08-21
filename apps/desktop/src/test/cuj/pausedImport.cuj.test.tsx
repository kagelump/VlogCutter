/**
 * An import that pauses on a missing volume and continues by itself (#79).
 *
 * The journey #67 could not offer. There, a vanished source produced a dialog
 * and a Resume button, because the code could not tell a lid-close from an
 * ejected card. #76 made the stall detectable and #78 gave volumes an identity,
 * so the common case — lid shut, lid opened, same drives — needs no user at all.
 *
 * What these journeys prove is the pair of promises that makes that safe: it
 * continues on its own when the endpoint proves it is the same one, and it
 * never continues when it cannot. The second promise is the expensive one; a
 * wrong answer there copies onto someone else's disk.
 *
 * The harness has no filesystem and no volumes. Volume identity here is the id
 * the backend would report; nothing below is evidence about real `diskutil`
 * output or real copying.
 */

import { act, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { desktopScenario, queueFilenames, renderApp } from "../harness";

const CARD_PATH = "/Volumes/DJI_SD";
const ARCHIVE_ID = "archive_nas_video";

function cardInSlot() {
  return desktopScenario({ now: "2026-08-20T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 2, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" })
    .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 })
    .cardInserted("DJI_SD", { clips: 3, newClips: 3, camera: "DJI Pocket 3", cameraHint: "DJI Pocket 3" })
    .build();
}

/**
 * The paused state watches on an interval. A test may not wait it out, but it
 * must give the watch enough ticks to notice and to act on what it noticed.
 */
async function letTheWatchRun() {
  await act(async () => {
    vi.advanceTimersByTime(16_000);
  });
}

/** Start the import, commit one clip, then take the named endpoint away. */
async function importThenLose(
  harness: ReturnType<typeof cardInSlot>,
  endpoint: "source" | "destination",
) {
  const app = await renderApp(harness);
  await app.user.click(await screen.findByRole("button", { name: "Review import" }));
  const dialog = await screen.findByRole("dialog");
  const run = harness.job<void, Record<string, never>>("run_smart_import");

  await app.user.click(within(dialog).getByRole("button", { name: /Import 3 clips/ }));
  await run.started();
  await run.progress();
  await waitFor(() => expect(queueFilenames()).toContain("DJI_001.MP4"));

  const ejected = endpoint === "source" ? harness.card(CARD_PATH).eject() : null;
  if (endpoint === "destination") harness.archive(ARCHIVE_ID).disconnect();
  await run.complete();
  return { app, ejected };
}

describe("an import whose destination goes away", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("pauses instead of reporting a quiet finish, and keeps what it committed", async () => {
    const harness = cardInSlot();
    await importThenLose(harness, "destination");

    expect(await screen.findByRole("heading", { name: "Import paused" })).toBeVisible();
    expect(screen.getByText(/NAS Video isn't connected/)).toBeVisible();
    // The committed clip is in the project, and it stays there.
    expect(queueFilenames()).toContain("DJI_001.MP4");
    expect(queueFilenames()).not.toContain("DJI_002.MP4");
    // A pause is not a failure.
    expect(screen.queryByRole("alert")).toBeNull();
    expect(harness.calls("generate_missing_proxies")).toHaveLength(0);
  });

  /// The promise of the whole feature: the drive comes back and the import goes
  /// on, with nothing to press.
  it("continues on its own once the same drive is back", async () => {
    const harness = cardInSlot();
    await importThenLose(harness, "destination");
    await screen.findByRole("heading", { name: "Import paused" });

    harness.archive(ARCHIVE_ID).reconnect();
    await letTheWatchRun();

    // Re-planned rather than resumed from the stale plan, so the clip already
    // committed is classified as known instead of copied a second time.
    await waitFor(() => expect(harness.calls("preflight_import").length).toBeGreaterThan(1));
    await waitFor(() => expect(queueFilenames()).toContain("DJI_003.MP4"));
    expect(screen.queryByRole("heading", { name: "Import paused" })).toBeNull();
  });

  /// The case #78 exists for, and the one that costs the most to get wrong: a
  /// drive is mounted at the remembered path and is not ours.
  it("never continues onto a different drive at the remembered path", async () => {
    const harness = cardInSlot();
    await importThenLose(harness, "destination");
    await screen.findByRole("heading", { name: "Import paused" });
    const plansBefore = harness.calls("preflight_import").length;

    harness.archive(ARCHIVE_ID).swapForDifferentVolume("vol_someone_elses");
    await letTheWatchRun();

    expect(screen.getByRole("heading", { name: "Import paused" })).toBeVisible();
    expect(
      screen.getByText("The drive connected as NAS Video isn't the one this import was using"),
    ).toBeVisible();
    expect(harness.calls("preflight_import")).toHaveLength(plansBefore);
    expect(queueFilenames()).not.toContain("DJI_002.MP4");
  });

  /// Escalation (#79 §5) is offered only for the condition a user can settle.
  /// An unplugged drive needs a cable, not a file picker.
  it("escalates to Locate archive… only when identity is the problem", async () => {
    const harness = cardInSlot();
    await importThenLose(harness, "destination");
    await screen.findByRole("heading", { name: "Import paused" });
    expect(screen.queryByRole("button", { name: /Locate archive/ })).toBeNull();

    harness.archive(ARCHIVE_ID).swapForDifferentVolume("vol_someone_elses");
    await letTheWatchRun();

    expect(await screen.findByRole("button", { name: /Locate archive/ })).toBeEnabled();
  });

  it("offers cancelling as its one routine action, and nothing to re-check", async () => {
    const harness = cardInSlot();
    await importThenLose(harness, "destination");
    await screen.findByRole("heading", { name: "Import paused" });

    expect(screen.getByRole("button", { name: "Cancel import" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: /check again/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /Resume import/ })).toBeNull();
  });

  /// Elapsed time is not a condition. A pause left overnight is judged by the
  /// same rule as one left for a moment.
  it("continues after a long pause exactly as it would after a short one", async () => {
    const harness = cardInSlot();
    await importThenLose(harness, "destination");
    await screen.findByRole("heading", { name: "Import paused" });

    await act(async () => {
      vi.advanceTimersByTime(8 * 60 * 60 * 1_000);
    });
    expect(screen.getByRole("heading", { name: "Import paused" })).toBeVisible();

    harness.archive(ARCHIVE_ID).reconnect();
    await letTheWatchRun();

    await waitFor(() => expect(queueFilenames()).toContain("DJI_003.MP4"));
  });

  it("releases the shell: unrelated review work is not left disabled", async () => {
    const harness = cardInSlot();
    await importThenLose(harness, "destination");
    await screen.findByRole("heading", { name: "Import paused" });

    expect(screen.getByRole("button", { name: /Add media/ })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
  });

  it("stops cleanly when cancelled, keeping the clips it committed", async () => {
    const harness = cardInSlot();
    const { app } = await importThenLose(harness, "destination");
    await screen.findByRole("heading", { name: "Import paused" });

    await app.user.click(screen.getByRole("button", { name: "Cancel import" }));

    await waitFor(() =>
      expect(screen.queryByRole("heading", { name: "Import paused" })).toBeNull(),
    );
    expect(queueFilenames()).toContain("DJI_001.MP4");

    // Cancelling is final: a drive coming back afterwards does not restart it.
    harness.archive(ARCHIVE_ID).reconnect();
    await letTheWatchRun();
    expect(screen.queryByRole("heading", { name: "Import paused" })).toBeNull();
    expect(queueFilenames()).not.toContain("DJI_002.MP4");
  });
});

describe("an import whose source goes away", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("pauses on the card and names it", async () => {
    const harness = cardInSlot();
    await importThenLose(harness, "source");

    expect(await screen.findByRole("heading", { name: "Import paused" })).toBeVisible();
    expect(screen.getByText(/DJI_SD isn't connected/)).toBeVisible();
    expect(queueFilenames()).toContain("DJI_001.MP4");
  });

  /// The #67 journey, minus the Resume click that #79 removes.
  it("continues on its own when the same card returns, even at a new path", async () => {
    const harness = cardInSlot();
    const { ejected } = await importThenLose(harness, "source");
    await screen.findByRole("heading", { name: "Import paused" });

    harness.card(CARD_PATH).remount(ejected!, "/Volumes/DJI_SD 1");
    await letTheWatchRun();

    await waitFor(() => {
      const [latest] = harness.calls("preflight_import").slice(-1);
      expect(latest?.sourcePath).toBe("/Volumes/DJI_SD 1");
    });
    await waitFor(() => expect(queueFilenames()).toContain("DJI_003.MP4"));
  });

  /// Identity, not the slot. A different card in the same reader is not this
  /// import's source and must never be copied from.
  it("refuses a different card in the same slot", async () => {
    const harness = cardInSlot();
    await importThenLose(harness, "source");
    await screen.findByRole("heading", { name: "Import paused" });
    const plansBefore = harness.calls("preflight_import").length;

    harness.card(CARD_PATH).swapForDifferentCard("card_someone_elses");
    await letTheWatchRun();

    expect(screen.getByRole("heading", { name: "Import paused" })).toBeVisible();
    expect(harness.calls("preflight_import")).toHaveLength(plansBefore);
    expect(queueFilenames()).not.toContain("DJI_002.MP4");
  });
});
