/**
 * CUJ 2 and 3: the judgment loop, and writing through a presentation change.
 *
 * Every assertion here reads the backend after the write. A note that reaches
 * React state and not the catalog is the failure this file exists to catch, and
 * it is invisible to a test that asserts on a spy.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, queueFilenames, renderApp, selectedRow } from "../harness";

function session() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 4, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" })
    .build();
}

const first = "asset_sony_a7c_ii_001";
const second = "asset_sony_a7c_ii_002";

describe("CUJ 2 — the judgment loop", () => {
  it("saves a note when the user leaves for another clip, and shows it again on return", async () => {
    const harness = session();
    const app = await renderApp(harness);

    await app.user.type(screen.getByLabelText("Notes"), "Station clock is legible here.");
    // Clicking the next clip is how a reviewer actually leaves the field.
    await app.user.click(screen.getByText("SONY_002.MP4"));

    await waitFor(() => expect(harness.state.assets[first].notes).toBe("Station clock is legible here."));
    await waitFor(() => expect(screen.getByLabelText("Notes")).toHaveValue(""));

    await app.user.click(screen.getByText("SONY_001.MP4"));
    await waitFor(() => expect(screen.getByLabelText("Notes")).toHaveValue("Station clock is legible here."));
  });

  it("saves and advances on Shift+Enter, the way a labelling pass is actually run", async () => {
    const harness = session();
    const app = await renderApp(harness);

    await app.user.click(screen.getByLabelText("Notes"));
    await app.user.keyboard("Arrival at the station{Shift>}{Enter}{/Shift}");

    await waitFor(() => expect(harness.state.assets[first].notes).toBe("Arrival at the station"));
    await waitFor(() => expect(selectedRow()?.textContent).toContain("SONY_002.MP4"));
  });

  it("discards the draft when the user backs out with Escape", async () => {
    const harness = session();
    const app = await renderApp(harness);

    await app.user.click(screen.getByLabelText("Notes"));
    await app.user.keyboard("half a thought{Escape}");

    await waitFor(() => expect(screen.getByLabelText("Notes")).toHaveValue(""));
    expect(harness.state.assets[first].notes).toBe("");
  });

  it("adds and removes labels through their own commands", async () => {
    const harness = session();
    const app = await renderApp(harness);

    await app.user.type(screen.getByLabelText("Add label"), "arrival{Enter}");
    await waitFor(() => expect(harness.state.assets[first].labels).toEqual(["arrival"]));

    await app.user.type(screen.getByLabelText("Add label"), "establishing{Enter}");
    await waitFor(() => expect(harness.state.assets[first].labels).toEqual(["arrival", "establishing"]));

    await app.user.click(screen.getByLabelText("Remove arrival"));
    await waitFor(() => expect(harness.state.assets[first].labels).toEqual(["establishing"]));
  });

  it("refuses a duplicate label without a backend write", async () => {
    const harness = session();
    const app = await renderApp(harness);

    await app.user.type(screen.getByLabelText("Add label"), "arrival{Enter}");
    await waitFor(() => expect(harness.state.assets[first].labels).toEqual(["arrival"]));
    const writes = harness.calls("update_asset").length;

    await app.user.type(screen.getByLabelText("Add label"), "Arrival{Enter}");
    await waitFor(() => expect(screen.getByLabelText("Add label")).toHaveValue(""));

    expect(harness.calls("update_asset")).toHaveLength(writes);
    expect(harness.state.assets[first].labels).toEqual(["arrival"]);
  });

  it("removes a clip, shows it under the removed view, and restores it in place", async () => {
    const harness = session();
    const app = await renderApp(harness);

    await app.user.keyboard("j");
    await waitFor(() => expect(selectedRow()?.textContent).toContain("SONY_002.MP4"));
    await app.user.keyboard("r");

    await waitFor(() => expect(harness.state.assets[second].removed).toBe(true));
    expect(queueFilenames()).toEqual(["SONY_001.MP4", "SONY_003.MP4", "SONY_004.MP4"]);

    await app.user.click(screen.getByRole("button", { name: /Removed 1/ }));
    await waitFor(() => expect(queueFilenames()).toEqual(["SONY_002.MP4"]));

    await app.user.click(screen.getByRole("button", { name: "Restore clip" }));
    await waitFor(() => expect(harness.state.assets[second].removed).toBe(false));
    await waitFor(() => expect(queueFilenames()).toEqual(["SONY_001.MP4", "SONY_002.MP4", "SONY_003.MP4", "SONY_004.MP4"]));
  });

  it("moves the selection off a clip it just removed", async () => {
    const harness = session();
    const app = await renderApp(harness);

    await app.user.keyboard("r");
    await waitFor(() => expect(harness.state.assets[first].removed).toBe(true));
    expect(selectedRow()?.textContent).toContain("SONY_002.MP4");
  });

  it("promotes an AI suggestion into a real label the user owns", async () => {
    const harness = desktopScenario()
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 2, withProxy: true })
      .withAnalysis("asset_sony_a7c_ii_001", { aiLabels: ["station", "arrival"] })
      .build();
    const app = await renderApp(harness);

    await app.user.click(await screen.findByRole("button", { name: "station" }));

    await waitFor(() => expect(harness.state.assets[first].labels).toEqual(["station"]));
    // A suggestion you took stops being a suggestion.
    expect(screen.queryByRole("button", { name: "station" })).toBeNull();
    expect(within(screen.getByLabelText("Clip labels")).getByText("station")).toBeVisible();
  });

  it("writes a clip title through the summary command, not the asset patch", async () => {
    const harness = session();
    const app = await renderApp(harness);

    await app.user.type(screen.getByLabelText("Clip title"), "Train pulls into Otaru station{Enter}");

    await waitFor(() => expect(harness.state.assets[first].title).toBe("Train pulls into Otaru station"));
    expect(harness.calls("set_clip_summary").at(-1)).toEqual({
      assetId: first,
      title: "Train pulls into Otaru station",
      description: "",
    });
  });
});

describe("CUJ 3 — writing through Focus", () => {
  it("keeps the same textarea, the draft, and the selection across the transition", async () => {
    const harness = session();
    const app = await renderApp(harness);

    const notes = screen.getByLabelText("Notes");
    await app.user.click(notes);
    await app.user.keyboard("The light changes about here");

    // F is a review shortcut, so leave the field before pressing it.
    notes.blur();
    await app.user.keyboard("f");
    await waitFor(() => expect(document.querySelector(".app-shell")).toHaveClass("is-focus"));

    // Same element, not a remount: this is what keeps the caret and the
    // playback position alive across the change.
    expect(screen.getByLabelText("Notes")).toBe(notes);
    expect(notes).toHaveValue("The light changes about here");
    expect(selectedRow ()?.textContent ?? "").toContain("SONY_001.MP4");

    await app.user.keyboard(" and again at the end");
    await app.user.keyboard("{Escape}");

    await waitFor(() => expect(document.querySelector(".app-shell")).not.toHaveClass("is-focus"));
    await waitFor(() =>
      expect(harness.state.assets[first].notes).toBe("The light changes about here and again at the end"),
    );
  });
});
