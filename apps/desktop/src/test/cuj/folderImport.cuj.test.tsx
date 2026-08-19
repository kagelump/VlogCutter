/**
 * CUJ: "Add media" — importing a folder that is not a camera card.
 *
 * The card path has CUJs; the folder path had none, even though it is the
 * ordinary way media already on disk gets into a project. `importFolder`,
 * `locateImportArchive` and `closeImportDialog` in `App.tsx` were unreached,
 * along with `chooseFolder`, `inspectImportSource` and `bindArchiveRoot` in
 * `api.ts`.
 *
 * The distinction that matters: a folder is not a card, so nothing is copied
 * and no archive destination is asked for. Originals stay where they are. A
 * regression that started copying a referenced folder into an archive would
 * duplicate a user's library silently, and until now nothing would have caught
 * it.
 *
 * The picker itself is the declared `plugin:dialog|open` seam, not evidence
 * that macOS shows a folder chooser — that stays a native acceptance item.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { buildCameraAssets, desktopScenario, queueFilenames, renderApp } from "../harness";

const PROJECT = "project_hokkaido";
const FOLDER = "/Users/harness/Movies/Hokkaido raw";

/** A project, plus two clips sitting in a folder the user is about to add. */
function folderOfClips() {
  return desktopScenario({ now: "2026-08-14T09:00:00.000Z" })
    .project("Hokkaido field test", { id: PROJECT })
    .camera("Sony A7C II", { clips: 1 })
    .dialogAnswers(FOLDER)
    .tap((state) => {
      state.imports.additions[FOLDER] = buildCameraAssets({
        projectId: PROJECT,
        camera: "GoPro Hero 12",
        spec: { clips: 2, startAt: "2026-02-09T02:00:00.000Z", volume: FOLDER },
      });
    })
    .build();
}

async function addMedia(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(screen.getByRole("button", { name: /Add media/ }));
  return screen.findByRole("dialog");
}

describe("adding a folder of existing media", () => {
  it("asks the picker for a folder and previews what it found", async () => {
    const harness = folderOfClips();
    const app = await renderApp(harness);

    const dialog = await addMedia(app);

    await waitFor(() => expect(harness.calls("preflight_import").length).toBeGreaterThan(0));
    // A folder is inspected for a card and turns out not to be one.
    expect(harness.calls("inspect_import_source")).toEqual([{ path: FOLDER }]);
    expect(harness.calls("preflight_import")[0]).toEqual({
      projectId: PROJECT,
      sourcePath: FOLDER,
      archiveRootId: null,
      cameraOverride: null,
    });
    expect(within(dialog).getByText(/Originals stay where they are/)).toBeVisible();
  });

  it("offers no archive destination, because nothing is being copied", async () => {
    const harness = folderOfClips();
    const app = await renderApp(harness);

    const dialog = await addMedia(app);
    await waitFor(() => expect(harness.calls("preflight_import").length).toBeGreaterThan(0));

    // The destination controls belong to the card path. A folder import that
    // grew one would be copying a library the user already has on disk.
    expect(within(dialog).queryByLabelText(/Originals destination/)).toBeNull();
    expect(within(dialog).queryByRole("button", { name: /New destination…/ })).toBeNull();
    expect(within(dialog).queryByLabelText(/Verify copies/)).toBeNull();
  });

  it("adds the folder's clips to the queue and leaves the originals in place", async () => {
    const harness = folderOfClips();
    const app = await renderApp(harness);
    const dialog = await addMedia(app);
    await waitFor(() => expect(harness.calls("preflight_import").length).toBeGreaterThan(0));

    await app.user.click(within(dialog).getByRole("button", { name: /Import 2 clips/ }));

    await waitFor(() => expect(queueFilenames()).toHaveLength(3));
    // The paths the project records are the folder's own, not an archive's.
    const added = Object.values(harness.state.assets).filter((asset) => asset.path.startsWith(FOLDER));
    expect(added).toHaveLength(2);
    expect(harness.state.imports.sessions.import_1.destinationMode).toBe("reference");
  });

  it("does nothing at all when the picker is dismissed", async () => {
    const harness = desktopScenario({ now: "2026-08-14T09:00:00.000Z" })
      .project("Hokkaido field test", { id: PROJECT })
      .camera("Sony A7C II", { clips: 1 })
      .build();
    const app = await renderApp(harness);
    harness.clearLog();

    await app.user.click(screen.getByRole("button", { name: /Add media/ }));

    await waitFor(() => expect(harness.commandNames()).toContain("plugin:dialog|open"));
    // No dialog, no scan, no state — cancelling a picker is not an import.
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(harness.commandNames()).not.toContain("preflight_import");
    expect(queueFilenames()).toHaveLength(1);
  });
});

describe("closing the import dialog", () => {
  it("cancels a scan still in flight rather than leaving it running", async () => {
    const harness = folderOfClips();
    harness.faults.always("preflight_import", { kind: "delay" });
    const app = await renderApp(harness);
    const dialog = await addMedia(app);
    await within(dialog).findByRole("status");

    await app.user.click(within(dialog).getByRole("button", { name: "Cancel" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    // A scan nobody is waiting for is work the machine should stop doing.
    expect(harness.commandNames()).toContain("cancel_import");
  });

  it("cancels nothing when the scan already finished", async () => {
    const harness = folderOfClips();
    const app = await renderApp(harness);
    const dialog = await addMedia(app);
    await waitFor(() => expect(harness.calls("preflight_import").length).toBeGreaterThan(0));
    harness.clearLog();

    await app.user.click(within(dialog).getByRole("button", { name: "Cancel" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    // Nothing is in flight, so closing is just closing: a stray `cancel_import`
    // here would race whatever the user does next.
    expect(harness.commandNames()).not.toContain("cancel_import");
    expect(queueFilenames()).toHaveLength(1);
  });
});

describe("an archive that has moved", () => {
  it("relocates the archive root and re-plans against the new mount", async () => {
    const harness = desktopScenario({ now: "2026-08-14T09:00:00.000Z" })
      .project("Hokkaido field test", { id: PROJECT })
      .camera("Sony A7C II", { clips: 1 })
      .archiveRoot("Shuttle SSD", { available: false, localMountPath: "" })
      .cardInserted("GOPRO_SD", { clips: 2, newClips: 2, camera: "GoPro Hero 12" })
      .dialogAnswers("/Volumes/Shuttle SSD")
      .build();
    const app = await renderApp(harness);
    await app.user.click(await screen.findByRole("button", { name: "Review import" }));
    const dialog = await screen.findByRole("dialog");
    await waitFor(() => expect(harness.calls("preflight_import").length).toBeGreaterThan(0));

    await app.user.click(within(dialog).getByRole("button", { name: /Locate Shuttle SSD…/ }));

    await waitFor(() =>
      expect(harness.calls("bind_archive_root")).toEqual([
        { archiveRootId: "archive_shuttle_ssd", path: "/Volumes/Shuttle SSD" },
      ]),
    );
    // Rebinding is only useful if the plan is recomputed against it: the
    // destination was blocked before and must not stay blocked after.
    expect(harness.state.archiveRoots[0]).toMatchObject({
      localMountPath: "/Volumes/Shuttle SSD",
      available: true,
    });
    await waitFor(() =>
      expect(harness.calls("preflight_import").at(-1)).toMatchObject({
        archiveRootId: "archive_shuttle_ssd",
      }),
    );
  });

  it("leaves the root alone when the locate picker is dismissed", async () => {
    const harness = desktopScenario({ now: "2026-08-14T09:00:00.000Z" })
      .project("Hokkaido field test", { id: PROJECT })
      .camera("Sony A7C II", { clips: 1 })
      .archiveRoot("Shuttle SSD", { available: false, localMountPath: "" })
      .cardInserted("GOPRO_SD", { clips: 2, newClips: 2, camera: "GoPro Hero 12" })
      .build();
    const app = await renderApp(harness);
    await app.user.click(await screen.findByRole("button", { name: "Review import" }));
    const dialog = await screen.findByRole("dialog");
    await waitFor(() => expect(harness.calls("preflight_import").length).toBeGreaterThan(0));

    await app.user.click(within(dialog).getByRole("button", { name: /Locate Shuttle SSD…/ }));

    await waitFor(() => expect(harness.commandNames()).toContain("plugin:dialog|open"));
    expect(harness.commandNames()).not.toContain("bind_archive_root");
    expect(harness.state.archiveRoots[0].available).toBe(false);
  });

  it("reports a rebind the backend refused, inside the dialog", async () => {
    const harness = desktopScenario({ now: "2026-08-14T09:00:00.000Z" })
      .project("Hokkaido field test", { id: PROJECT })
      .camera("Sony A7C II", { clips: 1 })
      .archiveRoot("Shuttle SSD", { available: false, localMountPath: "" })
      .cardInserted("GOPRO_SD", { clips: 2, newClips: 2, camera: "GoPro Hero 12" })
      .dialogAnswers("/Volumes/Wrong Disk")
      .build();
    harness.faults.always("bind_archive_root", {
      kind: "reject",
      message: "that folder is not the archive it claims to be",
    });
    const app = await renderApp(harness);
    await app.user.click(await screen.findByRole("button", { name: "Review import" }));
    const dialog = await screen.findByRole("dialog");
    await waitFor(() => expect(harness.calls("preflight_import").length).toBeGreaterThan(0));

    await app.user.click(within(dialog).getByRole("button", { name: /Locate Shuttle SSD…/ }));

    expect(await within(dialog).findByText(/not the archive it claims to be/)).toBeVisible();
    expect(harness.state.archiveRoots[0].available).toBe(false);
  });
});
