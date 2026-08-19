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

/** Same as `openImport`, without assuming the card is named "DJI_SD". */
async function openCardImport(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(await screen.findByRole("button", { name: "Review import" }));
  return screen.findByRole("dialog");
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

describe("choosing and remembering a camera identity (#71)", () => {
  /**
   * The regression this issue's review caught: an ordinary import the user
   * never corrects (a plain "detected" identity) must record nothing,
   * because that guess can differ from what a real, EXIF-aware ingest would
   * assign — recording it would let a later reinsertion silently force the
   * wrong name onto every asset. Mirrors
   * `core/rcdvlog-core/src/preflight.rs`'s
   * `a_detected_identity_is_never_recorded_but_a_user_choice_is_and_wins_over_real_exif`.
   */
  it("never records a mapping for a plain, uncorrected import", async () => {
    const scenario = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 })
      .cardInserted("SONY_SD", { clips: 1, newClips: 1, camera: "Sony (C-series)", cameraHint: "Sony", exifHint: "SONY ILCE-7CM2" });
    const harness = scenario.build();
    const app = await renderApp(harness);
    const dialog = await openCardImport(app);

    expect(within(dialog).getByText("Sony (C-series)", { selector: "strong" })).toBeVisible();
    await app.user.click(within(dialog).getByRole("button", { name: /Import 1 clip/ }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    expect(harness.state.imports.cameraMappings.project_hokkaido ?? []).toHaveLength(0);
  });

  /**
   * The issue's named CUJ, end to end through the real ImportDialog and the
   * real command names: wrong auto-detection, a deliberate correction,
   * import, then a reformatted-and-reinserted card — at a different mount
   * path — defaults to the corrected identity without the user repeating
   * themselves. `app.remount()` stands in for an app restart the way this
   * suite already uses it elsewhere: it proves what the frontend restores
   * from the backend it is talking to, not SQLite durability, which
   * `core/rcdvlog-core/src/preflight.rs`'s own
   * `a_remembered_mapping_survives_a_reformatted_card_a_new_mount_path_and_a_restart`
   * test owns.
   */
  it("wrong auto-detection, chosen once, is remembered for a reinserted card at a new mount path", async () => {
    // C####.MP4 only weakly identifies "Sony (C-series)" — a manufacturer
    // family, not the body the user actually owns. This is the issue's own
    // example of a wrong auto-detection.
    const scenario = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 1, withProxy: true })
      .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 })
      .cardInserted("SONY_SD", { clips: 3, newClips: 3, camera: "Sony (C-series)", cameraHint: "Sony", exifHint: "SONY ILCE-7CM2" });
    const harness = scenario.build();
    const preexistingAssetIds = new Set(Object.keys(harness.state.assets));

    let app = await renderApp(harness);
    let dialog = await openCardImport(app);

    expect(within(dialog).getByText("Sony (C-series)", { selector: "strong" })).toBeVisible();
    expect(within(dialog).getByText("Detected from this card.")).toBeVisible();

    // Choose the existing physical-camera identity rather than creating a
    // second lane for this card's weak filename guess.
    await app.user.selectOptions(within(dialog).getByLabelText("Change camera"), "Sony A7C II");
    await waitFor(() => expect(within(dialog).getByText("Sony A7C II", { selector: "strong" })).toBeVisible());
    expect(within(dialog).getByText(/Its camera metadata can be remembered/)).toBeVisible();

    // Import: every new asset lands under the chosen camera, not the weak
    // pattern guess — the sequence preview and asset ingest cannot disagree.
    await app.user.click(within(dialog).getByRole("button", { name: /Import 3 clips/ }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    const firstBatch = Object.values(harness.state.assets).filter((asset) => !preexistingAssetIds.has(asset.id));
    expect(firstBatch).toHaveLength(3);
    for (const asset of firstBatch) expect(asset.camera).toBe("Sony A7C II");

    // Reinsert a matching new card — a different mount path, as a
    // reformat/reinsertion would produce — and restart the app.
    const firstMountPath = harness.state.cards[0]?.mountPath;
    harness.state.cards = [];
    // A distinct fixture camera label so the harness's deterministic ids and
    // fingerprints ("asset_<slug>_<counter>") don't collide with the first
    // batch's; the filename prefix ("SONY") is what matters for evidence,
    // and it still comes from the same first word.
    scenario.cardInserted("SONY_SD_2", { clips: 1, newClips: 1, camera: "Sony (C-series) reformatted", cameraHint: "Sony", exifHint: "SONY ILCE-7CM2" });
    const secondMountPath = harness.state.cards[0]?.mountPath;
    const secondAssetId = harness.state.imports.additions[secondMountPath!]?.[0]?.id;
    expect(secondMountPath).not.toBe(firstMountPath);
    app = await app.remount();

    dialog = await openCardImport(app);
    expect(within(dialog).getByText("Sony A7C II", { selector: "strong" })).toBeVisible();
    expect(within(dialog).getByText("Remembered from a previous import with matching camera, card, and filename evidence.")).toBeVisible();

    await app.user.click(within(dialog).getByRole("button", { name: /Import 1 clip/ }));
    await waitFor(() => expect(Object.keys(harness.state.assets)).toHaveLength(5));
    const secondAsset = secondAssetId ? harness.state.assets[secondAssetId] : undefined;
    expect(secondAsset?.camera).toBe("Sony A7C II");
  });

  it("lets the user pick an existing project camera instead of typing a new one", async () => {
    const scenario = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 1, withProxy: true })
      .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 })
      .cardInserted("SONY_SD", { clips: 2, newClips: 2, camera: "Sony (C-series)", cameraHint: "Sony", exifHint: "SONY ILCE-7CM2" });
    const harness = scenario.build();
    const app = await renderApp(harness);
    const dialog = await openCardImport(app);

    await app.user.selectOptions(within(dialog).getByLabelText("Change camera"), "Sony A7C II");
    await waitFor(() => expect(within(dialog).getByText("Sony A7C II", { selector: "strong" })).toBeVisible());

    await app.user.click(within(dialog).getByRole("button", { name: /Import 2 clips/ }));
    await waitFor(() => expect(Object.keys(harness.state.assets)).toHaveLength(3));
    for (const asset of Object.values(harness.state.assets)) {
      if (asset.filename.startsWith("SONY_")) expect(asset.camera).toBe("Sony A7C II");
    }
  });

  it("cannot import a stale camera plan when replanning the choice fails", async () => {
    const harness = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 1, withProxy: true })
      .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 })
      .cardInserted("SONY_SD", {
        clips: 1,
        newClips: 1,
        camera: "Sony (C-series)",
        cameraHint: "Sony",
        exifHint: "SONY ILCE-7CM2",
      })
      .build();
    const app = await renderApp(harness);
    const dialog = await openCardImport(app);

    harness.next("preflight_import").reject("the source card disconnected");
    await app.user.selectOptions(within(dialog).getByLabelText("Change camera"), "Sony A7C II");

    expect(await within(dialog).findByRole("alert")).toHaveTextContent("the source card disconnected");
    expect(within(dialog).queryByRole("button", { name: /Import 1 clip/ })).toBeNull();
    expect(harness.calls("run_smart_import")).toHaveLength(0);
  });

  it("does not let a different camera under a generic label inherit a remembered mapping", async () => {
    // Ambiguous evidence: a plain `DCIM`-only card has no folder-layout hint
    // at all, so nothing here can be strong enough to remember or apply.
    const scenario = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 })
      .cardInserted("UNTITLED", { clips: 1, newClips: 1, camera: "Sony (C-series)", cameraHint: "" });
    const harness = scenario.build();
    const app = await renderApp(harness);
    const dialog = await openCardImport(app);

    expect(within(dialog).getByText("Sony (C-series)", { selector: "strong" })).toBeVisible();
    expect(
      within(dialog).getByText("Detected from the filenames — not much to go on, so double-check it."),
    ).toBeVisible();
  });

  it("lets the user inspect, change, and clear a stored mapping from any later import dialog", async () => {
    const scenario = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 })
      .cardInserted("SONY_SD", { clips: 1, newClips: 1, camera: "Sony (C-series)", cameraHint: "Sony", exifHint: "SONY ILCE-7CM2" });
    const harness = scenario.build();
    let app = await renderApp(harness);
    let dialog = await openCardImport(app);

    await app.user.selectOptions(within(dialog).getByLabelText("Change camera"), "New camera name…");
    await app.user.type(within(dialog).getByLabelText("New camera name"), "Sony A7C II");
    await app.user.click(within(dialog).getByRole("button", { name: "Use this name" }));
    await waitFor(() => expect(within(dialog).getByText("Sony A7C II", { selector: "strong" })).toBeVisible());
    await app.user.click(within(dialog).getByRole("button", { name: /Import 1 clip/ }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(harness.state.imports.cameraMappings.project_hokkaido).toHaveLength(1);

    // Open an unrelated later import dialog — the mapping is inspectable
    // from any of them, not only the one that created it.
    const folder = "/Users/harness/Movies/Extra footage";
    scenario.tap((state) => {
      state.dialogResponses.push(folder);
      state.imports.additions[folder] = [];
    });
    app = await app.remount();
    await app.user.click(screen.getByRole("button", { name: /Add media/ }));
    dialog = await screen.findByRole("dialog");

    await app.user.click(within(dialog).getByText("Remembered cameras (1)"));
    expect(within(dialog).getByText("Sony · SONY · SONY ILCE-7CM2")).toBeVisible();
    const input = within(dialog).getByLabelText("Camera for Sony · SONY · SONY ILCE-7CM2");
    expect(input).toHaveValue("Sony A7C II");

    // Change.
    await app.user.clear(input);
    await app.user.type(input, "Sony A7C II mk2");
    await app.user.click(within(dialog).getByRole("button", { name: "Save the remembered camera for Sony · SONY · SONY ILCE-7CM2" }));
    await waitFor(() =>
      expect(harness.state.imports.cameraMappings.project_hokkaido[0]?.camera).toBe("Sony A7C II mk2"),
    );
    expect(await within(dialog).findByDisplayValue("Sony A7C II mk2")).toBeVisible();

    // Clear.
    await app.user.click(
      within(dialog).getByRole("button", { name: "Clear the remembered camera for Sony · SONY · SONY ILCE-7CM2" }),
    );
    await waitFor(() => expect(harness.state.imports.cameraMappings.project_hokkaido).toHaveLength(0));
    expect(within(dialog).queryByText(/Remembered cameras/)).toBeNull();
  });
});
