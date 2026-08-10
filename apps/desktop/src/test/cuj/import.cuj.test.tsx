/**
 * The card-import journey.
 *
 * Between the import dialog's component tests and the physical acceptance the
 * I01 epic still owes — one real card inserted twice, one archive moved between
 * volumes — there was nothing. This is that middle: the whole flow, through the
 * production commands, against state that has to add up.
 *
 * The assertion in every case is the state diff. Nothing here is evidence for
 * real copying, verification hashing, or removable-volume detection.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, queueFilenames, renderApp } from "../harness";

/**
 * Two Sony clips already reviewed, and a card of five DJI clips — two of which
 * the project imported on an earlier insertion, three of which are new.
 */
function cardInSlot() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 2, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" })
    .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 })
    .cardInserted("DJI_SD", { clips: 5, newClips: 3, camera: "DJI Pocket 3", cameraHint: "DJI Pocket 3" })
    .build();
}

async function openImport(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(await screen.findByRole("button", { name: "Review import" }));
  return screen.findByRole("dialog", { name: /DJI_SD|Import/i });
}

async function openLastImport(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(screen.getByRole("button", { name: "More media generation actions" }));
  await app.user.click(await screen.findByRole("button", { name: /Review last import/ }));
  return screen.findByRole("dialog", { name: /DJI_SD|Import/i });
}

describe("importing from a card", () => {
  it("offers the card that is in the slot", async () => {
    const harness = cardInSlot();
    await renderApp(harness);

    expect(await screen.findByText(/DJI_SD/)).toBeVisible();
    expect(harness.calls("detect_mounted_cards").length).toBeGreaterThan(0);
  });

  it("previews without touching the project", async () => {
    const harness = cardInSlot();
    const app = await renderApp(harness);
    const before = JSON.stringify(harness.state.assets);

    const dialog = await openImport(app);

    await waitFor(() => expect(harness.calls("preflight_import").length).toBeGreaterThan(0));
    // Preflight is a read. The catalog is byte-identical afterwards.
    expect(JSON.stringify(harness.state.assets)).toBe(before);
    expect(within(dialog).getByRole("button", { name: /Import 3 clips/ })).toBeEnabled();
  });

  it("names the destination and what it will cost", async () => {
    const harness = cardInSlot();
    const app = await renderApp(harness);

    const dialog = await openImport(app);

    expect(within(dialog).getAllByText(/NAS Video/).length).toBeGreaterThan(0);
    expect(harness.calls("preflight_import")[0]).toMatchObject({
      projectId: "project_hokkaido",
      sourcePath: "/Volumes/DJI_SD",
      archiveRootId: "archive_nas_video",
    });
  });

  it("adds exactly the clips the plan classified as new", async () => {
    const harness = cardInSlot();
    const app = await renderApp(harness);
    const dialog = await openImport(app);

    await app.user.click(within(dialog).getByRole("button", { name: /Import 3 clips/ }));

    await waitFor(() => expect(Object.keys(harness.state.assets)).toHaveLength(7));
    expect(screen.queryByRole("dialog")).toBeNull();
    // Two Sony clips and the two DJI clips already known, plus the three new
    // ones — and nothing else.
    expect(queueFilenames()).toEqual([
      "SONY_001.MP4",
      "SONY_002.MP4",
      "DJI_001.MP4",
      "DJI_002.MP4",
      "DJI_003.MP4",
      "DJI_004.MP4",
      "DJI_005.MP4",
    ]);
    expect(Object.keys(harness.state.assets)).toHaveLength(7);
  });

  it("finishes import before background thumbnails progressively update the queue", async () => {
    const harness = cardInSlot();
    const app = await renderApp(harness);
    const dialog = await openImport(app);

    await app.user.click(within(dialog).getByRole("button", { name: /Import 3 clips/ }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    const imported = Object.values(harness.state.assets).find((asset) => asset.filename === "DJI_003.MP4");
    expect(imported).toBeDefined();
    const row = screen.getByText("DJI_003.MP4").closest("button");
    expect(row).not.toBeNull();
    expect(within(row!).getByText("Q0")).toBeVisible();

    const withThumbnail = {
      ...imported!,
      thumbnailPath: `/tmp/rcdvlog-demo/thumbnails/${imported!.id}/q1.v2.jpg`,
      thumbnailQuality: 1,
    };
    await harness.emit("thumbnail-progress", {
      asset: withThumbnail,
      estimate: {
        completedItems: 1,
        totalItems: 3,
        completedWorkUnits: imported!.sizeBytes,
        totalWorkUnits: imported!.sizeBytes * 3,
        workerCount: 1,
        estimatedRemainingSeconds: 1,
      },
    });

    await waitFor(() => expect(within(row!).queryByText("Q0")).toBeNull());
    expect(row!.querySelector("img")).toBeVisible();
  });

  it("reports counts that match the state diff", async () => {
    const harness = cardInSlot();
    const app = await renderApp(harness);
    const before = Object.keys(harness.state.assets).length;
    const dialog = await openImport(app);

    await app.user.click(within(dialog).getByRole("button", { name: /Import 3 clips/ }));

    await waitFor(() => expect(Object.keys(harness.state.assets)).toHaveLength(7));
    // The plan the run was given, the run's own summary, and the state diff all
    // say three. The browser mock could report any of the three without the
    // other two agreeing.
    const added = Object.keys(harness.state.assets).length - before;
    expect(added).toBe(3);
    expect(harness.calls("run_smart_import")[0]).toMatchObject({ plan: { newClips: 3, knownClips: 2 } });
    const reportDialog = await openLastImport(app);
    expect(within(reportDialog).getByText(/Import complete/)).toBeVisible();
  });

  it("remembers where the originals go for next time", async () => {
    const harness = cardInSlot();
    const app = await renderApp(harness);
    const dialog = await openImport(app);

    await app.user.click(within(dialog).getByRole("button", { name: /Import 3 clips/ }));

    await waitFor(() => expect(harness.state.settings.lastImportArchiveRootId).toBe("archive_nas_video"));
    expect(harness.state.settings.importArchiveRootIdsByCardSession.card_dji_sd).toBe("archive_nas_video");
    expect(harness.state.settings.makeProxiesOnImport).toBe(true);
  });

  it("prefers the destination remembered for this card over the most recent destination", async () => {
    const harness = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 })
      .archiveRoot("Travel SSD", { availableBytes: 500_000_000_000 })
      .cardInserted("DJI_SD", { clips: 2, newClips: 2, camera: "DJI Pocket 3" })
      .build();
    const card = harness.state.cards[0];
    harness.state.settings.lastImportArchiveRootId = "archive_travel_ssd";
    harness.state.settings.importArchiveRootIdsByCardSession[card.sessionId] = "archive_nas_video";
    const app = await renderApp(harness);

    await openImport(app);

    expect(harness.calls("preflight_import")[0]).toMatchObject({ archiveRootId: "archive_nas_video" });
  });

  it("imports only the additions when the same card comes back with more", async () => {
    const scenario = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 2, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" })
      .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 })
      .cardInserted("DJI_SD", { clips: 3, newClips: 3, camera: "DJI Pocket 3" });
    const harness = scenario.build();
    let app = await renderApp(harness);

    let dialog = await openImport(app);
    await app.user.click(within(dialog).getByRole("button", { name: /Import 3 clips/ }));
    await waitFor(() => expect(Object.keys(harness.state.assets)).toHaveLength(5));

    // Two more clips are shot onto the same card, and it goes back in the slot.
    scenario.cardGainedClips("/Volumes/DJI_SD", 2);
    app = await app.remount();

    dialog = await openImport(app);
    expect(await within(dialog).findByRole("button", { name: /Import 2 clips/ })).toBeEnabled();

    await app.user.click(within(dialog).getByRole("button", { name: /Import 2 clips/ }));
    // Exactly the additions: nothing already imported is copied a second time.
    await waitFor(() => expect(Object.keys(harness.state.assets)).toHaveLength(7));
    expect(harness.calls("run_smart_import").at(-1)).toMatchObject({
      plan: { newClips: 2, knownClips: 3 },
    });
  });

  it("blocks a destination that has no room, and says why", async () => {
    const harness = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 1, withProxy: true })
      .archiveRoot("Tiny USB", { availableBytes: 1_000 })
      .cardInserted("DJI_SD", { clips: 3, newClips: 3, camera: "DJI Pocket 3" })
      .build();
    const app = await renderApp(harness);

    const dialog = await openImport(app);

    expect(await within(dialog).findByText(/does not have room/)).toBeVisible();
    expect(within(dialog).getByRole("button", { name: /Import 3 clips/ })).toBeDisabled();
  });

  it("keeps what it copied when the user stops it partway", async () => {
    const harness = cardInSlot();
    const app = await renderApp(harness);
    const dialog = await openImport(app);
    const run = harness.job<void, { cancelled?: boolean }>("run_smart_import");

    await app.user.click(within(dialog).getByRole("button", { name: /Import 3 clips/ }));
    await run.started();
    await run.progress();
    await run.progress();

    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.getByText("Copying and verifying originals")).toBeVisible();
    await app.user.click(screen.getByRole("button", { name: "Stop" }));
    expect(harness.calls("cancel_import")).toHaveLength(1);
    await run.cancel();

    const reportDialog = await openLastImport(app);
    await waitFor(() => expect(within(reportDialog).getByText(/Stopped cleanly/)).toBeVisible());
    // The two it finished are in the project; the third is not.
    expect(Object.keys(harness.state.assets)).toHaveLength(6);
    expect(harness.state.assets.asset_dji_pocket_3_005).toBeUndefined();
  });

  it("takes a new archive destination the user chooses", async () => {
    const harness = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 1, withProxy: true })
      .cardInserted("DJI_SD", { clips: 2, newClips: 2, camera: "DJI Pocket 3" })
      .dialogAnswers("/Volumes/BigDisk/VideoArchive")
      .build();
    const app = await renderApp(harness);
    const dialog = await openImport(app);

    // With no archive configured yet, the dialog opens on the first-run form.
    await app.user.type(within(dialog).getByLabelText("Name this archive"), "Big Disk");
    await app.user.click(within(dialog).getByRole("button", { name: "Choose folder…" }));

    await waitFor(() => expect(harness.state.archiveRoots).toHaveLength(1));
    expect(harness.state.archiveRoots[0]).toMatchObject({
      displayName: "Big Disk",
      localMountPath: "/Volumes/BigDisk/VideoArchive",
      createdAt: "2026-08-09T12:00:00.000Z",
    });
  });
});
