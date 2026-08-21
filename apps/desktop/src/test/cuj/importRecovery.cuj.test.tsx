/**
 * Recovering an import whose source went away *before it started copying* (#67).
 *
 * The failure this covers is ordinary in the field: a bus-powered enclosure
 * browns out, a cable is nudged, a card is pulled a second early. The import
 * architecture already survives it — every file commits atomically, so the
 * bytes are safe (`docs/tasks/import/ARCHITECTURE.md` §4.4).
 *
 * ## What #79 took over, and what stayed here
 *
 * #79 supersedes the Resume workflow for a run that had *already begun*: a run
 * that reports a lost endpoint now pauses, watches for its volume, and
 * continues by itself with nothing to press. Those journeys live in
 * `pausedImport.cuj.test.tsx`.
 *
 * The boundary is whether a run exists to continue. A card that leaves during
 * preflight produced no run and committed nothing, so there is no partial copy
 * to resume — the honest recovery is to re-plan, which is the dialog #67 built.
 * That is what remains here, together with the answer to the question #79
 * raised about #67's re-entry point: it is not dead code. It is the escalation,
 * reached from the media menu and from `Choose source…`.
 *
 * The harness has no filesystem and no volumes. Card identity here is the
 * session id the backend would compute; nothing below is evidence about real
 * mount detection or real copying.
 */

import { act, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { desktopScenario, queueFilenames, renderApp } from "../harness";

const CARD_PATH = "/Volumes/DJI_SD";

/** Two Sony clips already in the project, and a card holding three new ones. */
function cardInSlot() {
  return desktopScenario({ now: "2026-08-20T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 2, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" })
    .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 })
    .cardInserted("DJI_SD", { clips: 3, newClips: 3, camera: "DJI Pocket 3", cameraHint: "DJI Pocket 3" })
    .build();
}

/**
 * The mount poll is a low-frequency interval; a test is not allowed to wait it
 * out. Long enough to cover the slow cycle too, since the poll tightens only
 * while a known source is missing.
 */
async function letTheMountPollRun() {
  await act(async () => {
    vi.advanceTimersByTime(16_000);
  });
}

describe("a source that disappears before the copy starts", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("recovers when the card leaves during preflight, before anything was copied", async () => {
    const harness = cardInSlot();
    const app = await renderApp(harness);
    harness.next("preflight_import").delayThenReject("Input/output error reading /Volumes/DJI_SD");

    await app.user.click(await screen.findByRole("button", { name: "Review import" }));
    const ejected = harness.card(CARD_PATH).eject();
    harness.release("preflight_import");

    // Reviewing the card dismissed its banner, so without recovery state the
    // card could come back with nothing left to offer.
    const dialog = await screen.findByRole("dialog");
    await waitFor(() => expect(within(dialog).getByText(/Source disconnected — waiting for/)).toBeVisible());
    expect(within(dialog).getByText(/Nothing was committed/)).toBeVisible();

    harness.card(CARD_PATH).remount(ejected);
    await letTheMountPollRun();
    await waitFor(() => expect(within(dialog).getByText("Source reconnected")).toBeVisible());
    await app.user.click(within(dialog).getByRole("button", { name: "Resume import" }));

    const resumed = await screen.findByRole("dialog");
    await waitFor(() =>
      expect(within(resumed).getByRole("button", { name: /Import 3 clips/ })).toBeEnabled(),
    );
  });

  /**
   * The answer to what becomes of #67's re-entry point once recovery is
   * automatic (#79): it is the escalation. A pre-copy interruption has no run
   * to continue, so it stays reachable here rather than in a paused state.
   */
  it("stays reachable from the media menu after the dialog is closed", async () => {
    const harness = cardInSlot();
    const app = await renderApp(harness);
    harness.next("preflight_import").delayThenReject("Input/output error reading /Volumes/DJI_SD");

    await app.user.click(await screen.findByRole("button", { name: "Review import" }));
    const ejected = harness.card(CARD_PATH).eject();
    harness.release("preflight_import");

    const dialog = await screen.findByRole("dialog");
    await waitFor(() => expect(within(dialog).getByText(/Source disconnected — waiting for/)).toBeVisible());
    await app.user.click(within(dialog).getByRole("button", { name: "Close import dialog" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    harness.card(CARD_PATH).remount(ejected);
    await letTheMountPollRun();

    await app.user.click(screen.getByRole("button", { name: "More media generation actions" }));
    const entry = await screen.findByRole("button", { name: /Resume interrupted import/ });
    expect(within(entry).getByText(/reconnected, ready to resume/)).toBeVisible();
    await app.user.click(entry);

    const resumed = await screen.findByRole("dialog");
    await waitFor(() =>
      expect(within(resumed).getByRole("button", { name: /Import 3 clips/ })).toBeEnabled(),
    );
  });
});
