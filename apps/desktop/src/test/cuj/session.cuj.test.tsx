/**
 * Whose answer is this?
 *
 * Every project read is asynchronous, and the user can leave a project while
 * one is still in flight. The invariant under test is one sentence: a response
 * to a read started for a project the user has left may not write anything —
 * not the queue, not the summaries, not the evidence, not the error, not the
 * loading state.
 *
 * The harness can hold an individual command by its arguments, so the race is
 * staged exactly rather than approximated with timers: project A's read is held
 * open, project B is opened and answered, and only then is A released.
 */

import { screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, queueFilenames, renderApp } from "../harness";

const A = "project_hokkaido";
const B = "project_okinawa";

function twoProjects() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: A })
    .camera("Sony A7C II", { clips: 3, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" })
    .withAnalysis("asset_sony_a7c_ii_001", { usable: false, unusableReason: "blurry" })
    .project("Okinawa reef", { id: B })
    .camera("DJI Pocket 3", { clips: 2, withProxy: true, startAt: "2026-03-02T00:10:00.000Z" })
    // Okinawa has a verdict of its own, so a stale index landing on top of it
    // would be visible as a change rather than as an absence.
    .withAnalysis("asset_dji_pocket_3_002", { usable: false, unusableReason: "blurry" })
    .build();
}

/** Open the other project through the topbar's switcher, as the user would. */
async function openProject(app: Awaited<ReturnType<typeof renderApp>>, projectId: string) {
  await app.user.selectOptions(screen.getByRole("combobox"), projectId);
}

describe("switching projects while a read is in flight", () => {
  it("does not let the first project's clips land in the second", async () => {
    const harness = twoProjects();
    // Hokkaido's queue read never answers until this test says so.
    harness.command("list_assets", (args) => args.projectId === A).delayUntilReleased();

    const app = await renderApp(harness);
    await openProject(app, B);

    await waitFor(() => expect(queueFilenames()).toEqual(["DJI_001.MP4", "DJI_002.MP4"]));

    // Hokkaido finally answers, into a session that has moved on.
    harness.command("list_assets").clear();
    harness.release("list_assets");

    await waitFor(() => expect(harness.calls("list_assets").length).toBeGreaterThan(1));
    expect(queueFilenames()).toEqual(["DJI_001.MP4", "DJI_002.MP4"]);
    expect(screen.getByRole("combobox")).toHaveValue(B);
  });

  it("does not let the first project's failure become the second's error", async () => {
    const harness = twoProjects();
    harness.command("list_assets", (args) => args.projectId === A)
      .delayThenReject("catalog unavailable: database is locked");

    const app = await renderApp(harness);
    await openProject(app, B);
    await waitFor(() => expect(queueFilenames()).toHaveLength(2));

    harness.command("list_assets").clear();
    harness.release("list_assets");
    await waitFor(() => expect(harness.calls("list_assets").length).toBeGreaterThan(1));

    // A stale rejection is not this project's news: no banner, no emptied
    // queue, and no retry offered for a project the user is not in.
    expect(screen.queryByText(/catalog unavailable/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
    expect(queueFilenames()).toEqual(["DJI_001.MP4", "DJI_002.MP4"]);
  });

  it("applies the same rule to the analysis index and the readiness summary", async () => {
    const harness = twoProjects();
    for (const command of ["chronology_overview", "project_analysis_index", "analysis_readiness"]) {
      harness.command(command, (args) => args.projectId === A).delayUntilReleased();
    }

    const app = await renderApp(harness);
    await openProject(app, B);
    await waitFor(() => expect(queueFilenames()).toHaveLength(2));
    // Okinawa: one of two clips judged unusable, one still to analyse.
    expect(await screen.findByRole("button", { name: "Unusable 1" })).toBeVisible();
    await app.user.click(screen.getByLabelText("More media generation actions"));
    expect(await screen.findByRole("button", { name: /Analyze 1 clips/ })).toBeVisible();

    for (const command of ["chronology_overview", "project_analysis_index", "analysis_readiness"]) {
      harness.command(command).clear();
      harness.release(command);
    }
    await waitFor(() => expect(harness.calls("project_analysis_index").length).toBeGreaterThan(1));

    // Hokkaido's index would show no unusable Okinawa clip and three clips to
    // analyse; neither number may appear over the project the user is in.
    expect(screen.getByRole("button", { name: "Unusable 1" })).toBeVisible();
    expect(screen.getByRole("button", { name: /Analyze 1 clips/ })).toBeVisible();
    expect(queueFilenames()).toEqual(["DJI_001.MP4", "DJI_002.MP4"]);
  });
});

describe("the selected clip's evidence", () => {
  it("belongs to the clip and the project that asked for it", async () => {
    const harness = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Hokkaido field test", { id: A })
      .camera("Sony A7C II", { clips: 2, withProxy: true })
      .withAnalysis("asset_sony_a7c_ii_001", { aiLabels: ["station"] })
      .project("Okinawa reef", { id: B })
      .camera("DJI Pocket 3", { clips: 2, withProxy: true })
      .build();
    harness.command("asset_analysis", (args) => args.assetId === "asset_sony_a7c_ii_001")
      .delayUntilReleased();

    const app = await renderApp(harness);
    await openProject(app, B);
    await waitFor(() => expect(queueFilenames()).toHaveLength(2));

    harness.command("asset_analysis").clear();
    harness.release("asset_analysis");
    await waitFor(() => expect(harness.calls("asset_analysis").length).toBeGreaterThan(1));

    // Hokkaido's suggestion cannot appear beside an Okinawa clip's title.
    expect(screen.queryByRole("button", { name: "station" })).toBeNull();
  });
});

describe("reloading the open project", () => {
  it("reads it again on request, without a write to force it", async () => {
    const harness = twoProjects();
    const app = await renderApp(harness);
    await waitFor(() => expect(queueFilenames()).toHaveLength(3));
    const before = harness.calls("list_assets").length;

    harness.asset("asset_sony_a7c_ii_002").patch({ notes: "changed on the other machine" });
    await app.user.click(screen.getByLabelText("Reload clips"));

    await waitFor(() => expect(harness.calls("list_assets").length).toBe(before + 1));
    expect(harness.calls("update_asset")).toHaveLength(0);
    await app.user.click(screen.getByText("SONY_002.MP4"));
    await waitFor(() =>
      expect(screen.getByLabelText("Notes")).toHaveValue("changed on the other machine"),
    );
  });
});
