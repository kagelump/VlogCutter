/**
 * "Everything before this clip is old." (#66)
 *
 * A card that has lived in a camera for months carries footage that predates
 * the project. This journey is the whole promise: say it once on a clip you
 * recognise, watch the numbers change, close the dialog, come back — and find
 * the rule still there, still naming the clip that set it, still clearable.
 *
 * The harness has no filesystem and no SQLite. What it does model faithfully
 * is where the rule lives (on harness state, not in a component, so it
 * survives a remount the way the real table survives a restart) and where it
 * is applied (before anything is derived from the candidates, as in
 * `preflight_import`).
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, queueFilenames, renderApp } from "../harness";

/** A long-lived card: two clips from before the trip, three from it. */
function reusedCard() {
  return desktopScenario({ now: "2026-08-20T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 1, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" })
    .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 })
    .cardInserted("DJI_SD", { clips: 5, newClips: 5, camera: "DJI Pocket 3", cameraHint: "DJI Pocket 3" })
    .build();
}

async function openCardImport(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(await screen.findByRole("button", { name: "Review import" }));
  return screen.findByRole("dialog");
}

async function openLedger(app: Awaited<ReturnType<typeof renderApp>>, dialog: HTMLElement) {
  const summary = within(dialog).getByText("Preview every file");
  if (!(summary.closest("details") as HTMLDetailsElement).open) await app.user.click(summary);
  return dialog;
}

async function importFrom(app: Awaited<ReturnType<typeof renderApp>>, dialog: HTMLElement, filename: string) {
  await openLedger(app, dialog);
  await app.user.click(
    within(dialog).getByRole("button", { name: new RegExp(`Import from ${filename} onward`) }),
  );
}

describe("a durable import cutoff for a reused card", () => {
  it("excludes everything before the chosen clip and says so in the same glance", async () => {
    const harness = reusedCard();
    const app = await renderApp(harness);
    const dialog = await openCardImport(app);
    await waitFor(() => expect(within(dialog).getByRole("button", { name: /Import 5 clips/ })).toBeEnabled());

    await importFrom(app, dialog, "DJI_003.MP4");

    await waitFor(() => expect(within(dialog).getByRole("button", { name: /Import 3 clips/ })).toBeEnabled());
    expect(within(dialog).getByText(/2 earlier clips excluded/)).toBeVisible();
    expect(within(dialog).getByText(/Starting at/)).toBeVisible();
    // Excluded is its own fact, never folded into "already known".
    const banner = within(dialog).getByTestId("import-cutoff-banner");
    expect(within(banner).getByText("Importing from DJI_003.MP4")).toBeVisible();
  });

  it("remembers the rule across closing the dialog, and across an app restart", async () => {
    const harness = reusedCard();
    let app = await renderApp(harness);
    let dialog = await openCardImport(app);
    await waitFor(() => expect(within(dialog).getByRole("button", { name: /Import 5 clips/ })).toBeEnabled());
    await importFrom(app, dialog, "DJI_003.MP4");
    await waitFor(() => expect(within(dialog).getByRole("button", { name: /Import 3 clips/ })).toBeEnabled());

    await app.user.click(within(dialog).getByRole("button", { name: "Close import dialog" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    // The renderer restarts. Nothing about the rule lived in the component.
    app = await app.remount();
    dialog = await openCardImport(app);
    await waitFor(() => expect(within(dialog).getByRole("button", { name: /Import 3 clips/ })).toBeEnabled());
    expect(within(dialog).getByText(/2 earlier clips excluded/)).toBeVisible();
    expect(within(dialog).getByText("DJI_003.MP4", { selector: ".import-summary-cutoff strong" })).toBeVisible();
  });

  it("imports only what the rule allows, and never reports the rest as a problem", async () => {
    const harness = reusedCard();
    const app = await renderApp(harness);
    const dialog = await openCardImport(app);
    await waitFor(() => expect(within(dialog).getByRole("button", { name: /Import 5 clips/ })).toBeEnabled());
    await importFrom(app, dialog, "DJI_003.MP4");
    await waitFor(() => expect(within(dialog).getByRole("button", { name: /Import 3 clips/ })).toBeEnabled());

    await app.user.click(within(dialog).getByRole("button", { name: /Import 3 clips/ }));

    await waitFor(() => expect(queueFilenames()).toContain("DJI_005.MP4"));
    expect(queueFilenames()).toEqual(["SONY_001.MP4", "DJI_003.MP4", "DJI_004.MP4", "DJI_005.MP4"]);
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByText(/Import complete · 3 clips added/)).toBeVisible();
  });

  it("moves to an earlier clip and then clears, offering the whole card again", async () => {
    const harness = reusedCard();
    const app = await renderApp(harness);
    const dialog = await openCardImport(app);
    await waitFor(() => expect(within(dialog).getByRole("button", { name: /Import 5 clips/ })).toBeEnabled());

    await importFrom(app, dialog, "DJI_004.MP4");
    await waitFor(() => expect(within(dialog).getByRole("button", { name: /Import 2 clips/ })).toBeEnabled());

    // Moving backwards means clicking a row the rule is currently hiding.
    await importFrom(app, dialog, "DJI_002.MP4");
    await waitFor(() => expect(within(dialog).getByRole("button", { name: /Import 4 clips/ })).toBeEnabled());

    await app.user.click(within(dialog).getByRole("button", { name: "Clear cutoff" }));
    await waitFor(() => expect(within(dialog).getByRole("button", { name: /Import 5 clips/ })).toBeEnabled());
    expect(within(dialog).queryByTestId("import-cutoff-banner")).toBeNull();
    expect(harness.state.imports.cutoffs.project_hokkaido).toEqual([]);
  });

  it("belongs to the card that set it, not to the camera that wrote it", async () => {
    const harness = reusedCard();
    const app = await renderApp(harness);
    const dialog = await openCardImport(app);
    await waitFor(() => expect(within(dialog).getByRole("button", { name: /Import 5 clips/ })).toBeEnabled());
    await importFrom(app, dialog, "DJI_003.MP4");
    await waitFor(() => expect(within(dialog).getByRole("button", { name: /Import 3 clips/ })).toBeEnabled());

    // A second card from the same camera arrives unfiltered: its first clip is
    // not old footage just because another card's was. Swapped before the
    // dialog closes, because closing it is what re-runs the mount poll.
    harness.card("/Volumes/DJI_SD").eject();
    harness.state.cards.push({
      sessionId: "card_dji_second",
      label: "DJI_SD_B",
      mountPath: "/Volumes/DJI_SD_B",
      cameraHint: "DJI Pocket 3",
      clipCount: 2,
      totalBytes: 2_000,
    });
    harness.state.imports.additions["/Volumes/DJI_SD_B"] = harness.state.imports.additions["/Volumes/DJI_SD"]
      .slice(0, 2)
      .map((asset, index) => ({ ...asset, id: `asset_second_${index}`, path: `/Volumes/DJI_SD_B/${asset.filename}` }));
    await app.user.click(within(dialog).getByRole("button", { name: "Close import dialog" }));

    const second = await openCardImport(app);
    await waitFor(() => expect(within(second).getByRole("button", { name: /Import 2 clips/ })).toBeEnabled());
    expect(within(second).queryByTestId("import-cutoff-banner")).toBeNull();
  });
});
