/**
 * CUJ: reviewing a project whose originals are not mounted.
 *
 * This is the designed mode, not an error state — a NAS project reviewed on a
 * laptop away from the NAS is exactly what proxies exist for — so what matters
 * is that the queue *states* it while you scan rather than letting you discover
 * it by hitting an operation that needs the source. The two offline states are
 * not the same fact: a clip with a proxy is reviewable and a clip without one
 * cannot be played at all.
 *
 * Repair is asserted from the clip that has the problem. Relink already existed
 * behind the chronology desk's diagnostics, which meant reaching it required
 * already knowing it was there.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, renderApp, selectedRow } from "../harness";

const sony = ["asset_sony_a7c_ii_001", "asset_sony_a7c_ii_002"];

/** Two clips whose volume is unmounted; the first still has its proxy. */
function unmountedVolume() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 2, withProxy: true, spacingSeconds: 600 })
    .offlineOriginal(sony[0], sony[1]);
}

function rowFor(filename: string) {
  return screen.getAllByRole("option").find((row) => row.textContent?.includes(filename));
}

describe("scanning a queue whose originals are offline", () => {
  it("marks the offline clips and leaves a mounted one unmarked", async () => {
    const harness = unmountedVolume()
      .camera("DJI Pocket 3", { clips: 1, withProxy: true })
      .build();
    await renderApp(harness);

    expect(rowFor("SONY_001.MP4")?.querySelector(".offline-pip")).not.toBeNull();
    expect(rowFor("DJI_001.MP4")?.querySelector(".offline-pip")).toBeNull();
  });

  it("says which offline state it is, in text the row actually carries", async () => {
    const harness = unmountedVolume().build();
    // The second clip's proxy is taken away, so the two states appear together.
    harness.state.assets[sony[1]].proxyAvailable = false;
    await renderApp(harness);

    expect(rowFor("SONY_001.MP4")?.textContent).toContain("reviewing from the 720p proxy");
    expect(rowFor("SONY_002.MP4")?.textContent).toContain("cannot be played");
    expect(rowFor("SONY_002.MP4")?.querySelector(".offline-pip.blocked")).not.toBeNull();
  });
});

describe("reconnecting the originals from the clip that lost them", () => {
  it("offers the relink workflow in the clip's own File shelf", async () => {
    const harness = unmountedVolume()
      .relinkableAt("/Volumes/Travel2", sony[0], sony[1])
      .dialogAnswers("/Volumes/Travel2")
      .build();
    const app = await renderApp(harness);

    await app.user.click(screen.getByRole("tab", { name: /File/ }));
    await app.user.click(await screen.findByRole("button", { name: "Locate originals…" }));

    await waitFor(() => expect(harness.state.assets[sony[0]].originalAvailable).toBe(true));
    // A volume comes back all at once, so the search repairs every clip it
    // recognises rather than only the selected one.
    expect(harness.state.assets[sony[1]].originalAvailable).toBe(true);
    expect(await screen.findByText("Reconnected 2 clips")).toBeVisible();
  });

  it("clears the queue's offline marks without a relaunch", async () => {
    const harness = unmountedVolume()
      .relinkableAt("/Volumes/Travel2", sony[0], sony[1])
      .dialogAnswers("/Volumes/Travel2")
      .build();
    const app = await renderApp(harness);
    expect(rowFor("SONY_001.MP4")?.querySelector(".offline-pip")).not.toBeNull();

    await app.user.click(screen.getByRole("tab", { name: /File/ }));
    await app.user.click(await screen.findByRole("button", { name: "Locate originals…" }));

    await waitFor(() => expect(rowFor("SONY_001.MP4")?.querySelector(".offline-pip")).toBeNull());
  });

  it("says nothing matched rather than implying a repair that did not happen", async () => {
    const harness = unmountedVolume()
      .dialogAnswers("/Volumes/Empty")
      .build();
    const app = await renderApp(harness);

    await app.user.click(screen.getByRole("tab", { name: /File/ }));
    await app.user.click(await screen.findByRole("button", { name: "Locate originals…" }));

    expect(await screen.findByText(/Nothing in that folder matched/)).toBeVisible();
    expect(harness.state.assets[sony[0]].originalAvailable).toBe(false);
  });

  it("leaves the project alone when the folder dialog is dismissed", async () => {
    const harness = unmountedVolume().dialogAnswers(null).build();
    const app = await renderApp(harness);
    await app.user.click(screen.getByRole("tab", { name: /File/ }));
    harness.clearLog();

    await app.user.click(await screen.findByRole("button", { name: "Locate originals…" }));

    expect(harness.commandNames()).not.toContain("relink_folder");
    expect(selectedRow()?.querySelector(".offline-pip")).not.toBeNull();
  });

  it("reports a relink the backend refused", async () => {
    const harness = unmountedVolume()
      .relinkableAt("/Volumes/Travel2", sony[0], sony[1])
      .dialogAnswers("/Volumes/Travel2")
      .build();
    const app = await renderApp(harness);
    harness.next("relink_folder").reject("volume disappeared again");

    await app.user.click(screen.getByRole("tab", { name: /File/ }));
    await app.user.click(await screen.findByRole("button", { name: "Locate originals…" }));

    expect(await screen.findByText(/volume disappeared again/)).toBeVisible();
    expect(harness.state.assets[sony[0]].originalAvailable).toBe(false);
  });

  it("offers nothing to locate on a clip whose original is mounted", async () => {
    const harness = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 1, withProxy: true })
      .build();
    const app = await renderApp(harness);

    await app.user.click(screen.getByRole("tab", { name: /File/ }));

    const shelf = await screen.findByRole("tabpanel");
    expect(within(shelf).queryByRole("button", { name: "Locate originals…" })).toBeNull();
  });
});
