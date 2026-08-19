/**
 * Recovering an import whose source went away (#67).
 *
 * The failure this covers is ordinary in the field: a bus-powered enclosure
 * browns out, a cable is nudged, a card is pulled a second early. The import
 * architecture already survives it — every file commits atomically, so the
 * bytes are safe (`docs/tasks/import/ARCHITECTURE.md` §4.4) — and what this
 * journey proves is the way back: the session stays reachable, the same card
 * is recognized on its own, a *different* card in the same slot is not, and
 * resuming copies only what is left.
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

/**
 * Start the import, commit one clip, then pull the card out from under it.
 * Returns the ejected card so the test can decide what comes back.
 */
async function importThenPullTheCard(harness: ReturnType<typeof cardInSlot>) {
  const app = await renderApp(harness);
  await app.user.click(await screen.findByRole("button", { name: "Review import" }));
  const dialog = await screen.findByRole("dialog");
  const run = harness.job<void, Record<string, never>>("run_smart_import");

  await app.user.click(within(dialog).getByRole("button", { name: /Import 3 clips/ }));
  await run.started();
  await run.progress();
  await waitFor(() => expect(queueFilenames()).toContain("DJI_001.MP4"));

  const ejected = harness.card(CARD_PATH).eject();
  await run.complete();
  return { app, ejected };
}

describe("a source that disappears mid-import", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("offers a way back instead of a generic error, and keeps what it committed", async () => {
    const harness = cardInSlot();
    await importThenPullTheCard(harness);

    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/Source disconnected — waiting for/)).toBeVisible();
    expect(within(dialog).getByText(/1 clip committed/)).toBeVisible();
    // Not an error, and not a dead end: every recovery action is on screen.
    expect(screen.queryByRole("alert")).toBeNull();
    expect(within(dialog).getByRole("button", { name: "Reconnect source" })).toBeDisabled();
    expect(within(dialog).getByRole("button", { name: "Choose source…" })).toBeEnabled();
    expect(within(dialog).getByRole("button", { name: "Cancel import" })).toBeEnabled();

    // The clip that committed before the card left is in the project.
    expect(queueFilenames()).toContain("DJI_001.MP4");
    expect(queueFilenames()).not.toContain("DJI_002.MP4");
    // Proxies are not attempted for a run that stopped this way.
    expect(harness.calls("generate_missing_proxies")).toHaveLength(0);
  });

  it("releases the shell: unrelated review work is not left disabled", async () => {
    const harness = cardInSlot();
    await importThenPullTheCard(harness);

    await screen.findByRole("dialog");
    expect(screen.getByRole("button", { name: /Add media/ })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
  });

  it("recognizes the same card coming back, even at a different mount path", async () => {
    const harness = cardInSlot();
    const { app, ejected } = await importThenPullTheCard(harness);
    await screen.findByRole("dialog");

    harness.card(CARD_PATH).remount(ejected, "/Volumes/DJI_SD 1");
    await letTheMountPollRun();

    const dialog = await screen.findByRole("dialog");
    await waitFor(() => expect(within(dialog).getByText("Source reconnected")).toBeVisible());
    await app.user.click(within(dialog).getByRole("button", { name: "Resume import" }));

    // Resume re-plans against the card that is actually there, and the clip
    // already committed is classified as known rather than copied twice.
    await waitFor(() => {
      const [latest] = harness.calls("preflight_import").slice(-1);
      expect(latest?.sourcePath).toBe("/Volumes/DJI_SD 1");
    });
    const resumed = await screen.findByRole("dialog");
    await waitFor(() =>
      expect(within(resumed).getByRole("button", { name: /Import 2 clips/ })).toBeEnabled(),
    );
  });

  it("preserves the destination and the import options across the interruption", async () => {
    const harness = cardInSlot();
    const { app, ejected } = await importThenPullTheCard(harness);
    await screen.findByRole("dialog");

    harness.card(CARD_PATH).remount(ejected);
    await letTheMountPollRun();
    await app.user.click(await screen.findByRole("button", { name: "Resume import" }));

    await waitFor(() => expect(harness.calls("preflight_import").length).toBeGreaterThan(1));
    const [first] = harness.calls("preflight_import");
    const [latest] = harness.calls("preflight_import").slice(-1);
    expect(latest?.archiveRootId).toBe(first?.archiveRootId);

    const resumed = await screen.findByRole("dialog");
    await waitFor(() =>
      expect(within(resumed).getByRole("button", { name: /Import 2 clips/ })).toBeEnabled(),
    );
    expect(within(resumed).getByLabelText(/Verify copies with a full-file hash/)).toBeChecked();
  });

  it("refuses to treat a different card in the same slot as the interrupted source", async () => {
    const harness = cardInSlot();
    await importThenPullTheCard(harness);
    await screen.findByRole("dialog");

    harness.card(CARD_PATH).swapForDifferentCard("card_someone_elses");
    await letTheMountPollRun();

    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByRole("button", { name: "Reconnect source" })).toBeDisabled();
    expect(within(dialog).getByText(/Source disconnected — waiting for/)).toBeVisible();
  });

  it("stays reachable from the media menu after the dialog is closed", async () => {
    const harness = cardInSlot();
    const { app, ejected } = await importThenPullTheCard(harness);

    const dialog = await screen.findByRole("dialog");
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
      expect(within(resumed).getByRole("button", { name: /Import 2 clips/ })).toBeEnabled(),
    );
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

  it("offers the card again from the banner once the interrupted session is cancelled", async () => {
    const harness = cardInSlot();
    const { app, ejected } = await importThenPullTheCard(harness);

    const dialog = await screen.findByRole("dialog");
    await app.user.click(within(dialog).getByRole("button", { name: "Cancel import" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    harness.card(CARD_PATH).remount(ejected);
    await letTheMountPollRun();

    // Cancelling returns to before the import, dismissal included: the card in
    // the slot is offerable again rather than silently unreachable.
    expect(await screen.findByRole("button", { name: "Review import" })).toBeVisible();
  });

  it("cancels safely from the disconnected state and stops offering the session", async () => {
    const harness = cardInSlot();
    const { app } = await importThenPullTheCard(harness);

    const dialog = await screen.findByRole("dialog");
    await app.user.click(within(dialog).getByRole("button", { name: "Cancel import" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    // Cancelling abandons the remainder, never the clips already committed.
    expect(queueFilenames()).toContain("DJI_001.MP4");
    await app.user.click(screen.getByRole("button", { name: "More media generation actions" }));
    expect(screen.queryByText("Resume interrupted import")).toBeNull();
  });
});
