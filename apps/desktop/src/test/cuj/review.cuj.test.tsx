/**
 * CUJ 1, 5, and 12: opening a project, moving through it, rotating a clip, and
 * coming back to it after the renderer restarts.
 *
 * These assert what the backend holds after each step, not what a spy was
 * called with. A mutation that never reached canonical state is the exact
 * failure this tier exists to catch.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, queueFilenames, renderApp, selectedRow } from "../harness";

function trip() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 3, withProxy: true, startAt: "2026-02-08T00:10:00.000Z", spacingSeconds: 600 })
    .camera("DJI Pocket 3", { clips: 2, withProxy: true, startAt: "2026-02-08T00:15:00.000Z", spacingSeconds: 600 })
    .withSummary("asset_sony_a7c_ii_001", "Train pulls into Otaru station")
    .withLabels("asset_sony_a7c_ii_002", "market")
    .build();
}

describe("CUJ 1 — open a project and review it", () => {
  it("lists every active clip in capture order and selects the first", async () => {
    const harness = trip();
    await renderApp(harness);

    // Interleaved by time, not grouped by camera: the queue is the trip.
    expect(queueFilenames()).toEqual([
      "SONY_001.MP4",
      "DJI_001.MP4",
      "SONY_002.MP4",
      "DJI_002.MP4",
      "SONY_003.MP4",
    ]);
    expect(selectedRow()?.textContent).toContain("Train pulls into Otaru station");
  });

  it("moves the selection with J and K and carries the whole screen with it", async () => {
    const harness = trip();
    const app = await renderApp(harness);

    await app.user.keyboard("j");
    await waitFor(() => expect(selectedRow()?.textContent).toContain("DJI_001.MP4"));
    // The right rail follows the selection, not the other way round.
    expect(screen.getByLabelText("Clip title")).toHaveValue("");
    expect(screen.getByLabelText("Notes")).toHaveValue("");

    await app.user.keyboard("k");
    await waitFor(() => expect(selectedRow()?.textContent).toContain("SONY_001.MP4"));
    expect(screen.getByLabelText("Clip title")).toHaveValue("Train pulls into Otaru station");
  });

  it("mounts the selected clip and its neighbours so stepping does not pay a load", async () => {
    const harness = trip();
    await renderApp(harness);

    const slots = [...document.querySelectorAll("video")].map((video) => video.getAttribute("data-cache-slot"));
    expect(slots).toContain("current");
    expect(slots).toContain("next");
  });

  it("records where the user is on every selection change", async () => {
    const harness = trip();
    const app = await renderApp(harness);
    harness.clearLog();

    await app.user.keyboard("j");
    await waitFor(() =>
      expect(harness.calls("record_desktop_context").at(-1)).toEqual({
        projectId: "project_hokkaido",
        assetId: "asset_dji_pocket_3_001",
      }),
    );
    expect(harness.state.settings.lastAssetId).toBe("asset_dji_pocket_3_001");
  });
});

describe("CUJ 5 — rotation survives leaving the clip", () => {
  it("writes the rotation and still shows it after navigating away and back", async () => {
    const harness = trip();
    const app = await renderApp(harness);

    await app.user.click(screen.getByLabelText("Frame orientation"));
    await app.user.click(await screen.findByRole("menuitemradio", { name: "90°" }));
    await waitFor(() => expect(harness.state.assets.asset_sony_a7c_ii_001.rotationDegrees).toBe(90));

    await app.user.keyboard("jk");
    await waitFor(() => expect(selectedRow()?.textContent).toContain("SONY_001.MP4"));

    // Read back from state through the UI: the stage's frame is turned.
    const active = document.querySelector<HTMLElement>('video[data-cache-slot="current"]')?.parentElement;
    expect(active?.className).toContain("quarter-turn");
    expect(harness.state.assets.asset_sony_a7c_ii_001.rotationDegrees).toBe(90);
  });

  it("sends the whole patch, because rotation is part of the asset contract", async () => {
    const harness = trip();
    const app = await renderApp(harness);

    await app.user.keyboard("o");
    await waitFor(() => expect(harness.calls("update_asset").length).toBeGreaterThan(0));

    expect(harness.calls("update_asset").at(-1)).toEqual({
      assetId: "asset_sony_a7c_ii_001",
      // The whole patch, and the revision it was decided from (issue #49).
      patch: { notes: "", labels: [], removed: false, rotationDegrees: 90, expectedRevision: 0 },
    });
  });
});

describe("CUJ 12 — the renderer restarts", () => {
  it("restores every judgment from the backend, not from React state", async () => {
    const harness = trip();
    let app = await renderApp(harness);

    await app.user.type(screen.getByLabelText("Notes"), "First clean view of the station clock.");
    await app.user.tab();
    await waitFor(() =>
      expect(harness.state.assets.asset_sony_a7c_ii_001.notes).toBe("First clean view of the station clock."),
    );

    await app.user.type(screen.getByLabelText("Add label"), "arrival{Enter}");
    await waitFor(() => expect(harness.state.assets.asset_sony_a7c_ii_001.labels).toEqual(["arrival"]));

    await app.user.click(screen.getByLabelText("Frame orientation"));
    await app.user.click(await screen.findByRole("menuitemradio", { name: "90°" }));
    await waitFor(() => expect(harness.state.assets.asset_sony_a7c_ii_001.rotationDegrees).toBe(90));

    app = await app.remount();

    expect(screen.getByLabelText("Notes")).toHaveValue("First clean view of the station clock.");
    expect(within(screen.getByLabelText("Clip labels")).getByText("arrival")).toBeVisible();
    expect(harness.state.assets.asset_sony_a7c_ii_001.rotationDegrees).toBe(90);
  });

  it("comes back to the clip the user was last on, not the first in the queue", async () => {
    const harness = trip();
    let app = await renderApp(harness);

    // Two clips in, to SONY_002 — well clear of the boot default (SONY_001).
    await app.user.keyboard("jj");
    await waitFor(() => expect(selectedRow()?.textContent).toContain("SONY_002.MP4"));
    await waitFor(() => expect(harness.state.settings.lastAssetId).toBe("asset_sony_a7c_ii_002"));

    app = await app.remount();

    // Restored to the recorded position rather than falling back to SONY_001.
    await waitFor(() => expect(selectedRow()?.textContent).toContain("SONY_002.MP4"));
  });

  it("falls back to the first clip when the recorded clip is gone, without an error", async () => {
    const harness = trip();
    harness.state.settings.lastAssetId = "asset_that_no_longer_exists";
    await renderApp(harness);

    expect(selectedRow()?.textContent).toContain("SONY_001.MP4");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("keeps the autoplay setting the user chose", async () => {
    const harness = trip();
    let app = await renderApp(harness);

    await app.user.click(screen.getByRole("button", { name: /Settings/ }));
    await app.user.click(await screen.findByRole("radio", { name: /Never/ }));
    await waitFor(() => expect(harness.state.settings.autoplayMode).toBe("never"));

    app = await app.remount();
    await app.user.click(screen.getByRole("button", { name: /Settings/ }));
    expect(await screen.findByRole("radio", { name: /Never/ })).toBeChecked();
  });
});
