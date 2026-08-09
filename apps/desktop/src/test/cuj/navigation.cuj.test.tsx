/**
 * CUJ 4: finding a clip again.
 *
 * Search reaches across everything the reviewer might remember — a filename, a
 * title analysis wrote, a word someone said on camera, a label they added — and
 * the two view toggles compose with it. The queue is not a list of files; it is
 * the only way back to a moment.
 */

import { screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, queueFilenames, renderApp, selectedRow } from "../harness";

function searchable() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 3, withProxy: true, startAt: "2026-02-08T00:10:00.000Z", spacingSeconds: 600 })
    .camera("DJI Pocket 3", { clips: 2, withProxy: true, startAt: "2026-02-08T00:15:00.000Z", spacingSeconds: 600 })
    .withSummary("asset_sony_a7c_ii_001", "Train pulls into Otaru station")
    .withTranscript("asset_dji_pocket_3_001", { text: "The market opens at six", language: "en" })
    .withLabels("asset_sony_a7c_ii_003", "market")
    .withNote("asset_dji_pocket_3_002", "Wind noise ruins the audio here")
    .withAnalysis("asset_sony_a7c_ii_002", { usable: false, unusableReason: "blurry" })
    .build();
}

function search() {
  return screen.getByPlaceholderText(/Filter titles/);
}

describe("CUJ 4 — search and filtering", () => {
  it("finds a clip by the title analysis gave it", async () => {
    const harness = searchable();
    const app = await renderApp(harness);

    await app.user.type(search(), "otaru");
    await waitFor(() => expect(queueFilenames()).toEqual(["SONY_001.MP4"]));
  });

  it("finds a clip by something said on camera", async () => {
    const harness = searchable();
    const app = await renderApp(harness);

    await app.user.type(search(), "market opens");
    await waitFor(() => expect(queueFilenames()).toEqual(["DJI_001.MP4"]));
  });

  it("finds a clip by a label the reviewer added", async () => {
    const harness = searchable();
    const app = await renderApp(harness);

    await app.user.type(search(), "market");
    // Both the spoken line and the label match: search is a union, not a mode.
    await waitFor(() => expect(queueFilenames()).toEqual(["DJI_001.MP4", "SONY_003.MP4"]));
  });

  it("finds a clip by a note the reviewer wrote", async () => {
    const harness = searchable();
    const app = await renderApp(harness);

    await app.user.type(search(), "wind noise");
    await waitFor(() => expect(queueFilenames()).toEqual(["DJI_002.MP4"]));
  });

  it("clearing the search restores the whole queue", async () => {
    const harness = searchable();
    const app = await renderApp(harness);

    await app.user.type(search(), "otaru");
    await waitFor(() => expect(queueFilenames()).toHaveLength(1));

    await app.user.clear(search());
    await waitFor(() => expect(queueFilenames()).toHaveLength(5));
  });

  it("narrows to the clips analysis judged unusable, and back", async () => {
    const harness = searchable();
    const app = await renderApp(harness);

    await app.user.click(await screen.findByRole("button", { name: /Unusable 1/ }));
    await waitFor(() => expect(queueFilenames()).toEqual(["SONY_002.MP4"]));

    await app.user.click(screen.getByRole("button", { name: "All clips" }));
    await waitFor(() => expect(queueFilenames()).toHaveLength(5));
  });

  it("composes the unusable filter with a search", async () => {
    const harness = searchable();
    const app = await renderApp(harness);

    await app.user.click(await screen.findByRole("button", { name: /Unusable 1/ }));
    await app.user.type(search(), "otaru");

    // SONY_001 matches the search but is usable; SONY_002 is unusable but does
    // not match. The intersection is empty, and the app says so rather than
    // silently dropping one of the two conditions.
    await waitFor(() => expect(queueFilenames()).toEqual([]));
    expect(screen.getByText("No clips here yet.")).toBeVisible();
  });

  it("keeps the selection legal when the filter excludes the selected clip", async () => {
    const harness = searchable();
    const app = await renderApp(harness);

    expect(selectedRow()?.textContent).toContain("Train pulls into Otaru station");
    await app.user.type(search(), "wind noise");

    await waitFor(() => expect(selectedRow()?.textContent).toContain("DJI_002.MP4"));
    expect(screen.getByLabelText("Notes")).toHaveValue("Wind noise ruins the audio here");
  });

  it("keeps removed clips out of search results until the removed view is on", async () => {
    const harness = searchable();
    const app = await renderApp(harness);

    await app.user.click(screen.getByText("DJI_002.MP4"));
    await app.user.keyboard("r");
    await waitFor(() => expect(harness.state.assets.asset_dji_pocket_3_002.removed).toBe(true));

    await app.user.type(search(), "wind noise");
    await waitFor(() => expect(queueFilenames()).toEqual([]));

    await app.user.click(screen.getByRole("button", { name: /Removed 1/ }));
    await waitFor(() => expect(queueFilenames()).toEqual(["DJI_002.MP4"]));
  });
});
