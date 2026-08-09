/**
 * CUJ 10 and 11: correcting a camera clock, and being stopped from doing damage.
 *
 * Every chronology value here is declared by the scenario. Whether a 60-minute
 * epoch correction really produces the ordering the scenario states is proven by
 * `cargo test -p rcdvlog-core` and by nothing in this file.
 *
 * What this file proves is the frontend's safety contract, which is entirely a
 * frontend property: preview before write, capture the user's place before
 * mutating, refresh after, and put them back where they were.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import {
  buildContradiction,
  buildDiagnostic,
  desktopScenario,
  renderApp,
  selectedRow,
} from "../harness";

const sony = ["asset_sony_a7c_ii_001", "asset_sony_a7c_ii_002"];
const dji = ["asset_dji_pocket_3_001"];

/**
 * A trip where the Sony's clock never left home time. The declared "after"
 * ordering is what the solver returns once the correction is applied.
 */
function miscalibrated() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 2, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" })
    .camera("DJI Pocket 3", { clips: 1, withProxy: true, startAt: "2026-02-08T00:20:00.000Z" })
    .chronology({
      order: [sony[0], sony[1], dji[0]],
      sequences: [
        { id: "sequence_sony", cameraKey: "Sony A7C II", label: "Sony A7C II", volumeHint: "/Volumes/Travel" },
        { id: "sequence_dji", cameraKey: "DJI Pocket 3", label: "DJI Pocket 3" },
      ],
    })
    .whenClockEpochApplied("sequence_sony")
    .thenChronology({
      order: [dji[0], sony[0], sony[1]],
      sequences: [
        { id: "sequence_sony", cameraKey: "Sony A7C II", label: "Sony A7C II", volumeHint: "/Volumes/Travel" },
        { id: "sequence_dji", cameraKey: "DJI Pocket 3", label: "DJI Pocket 3" },
      ],
    });
}

async function enterChronology(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(screen.getByRole("button", { name: "Chronology" }));
  return screen.findByRole("region", { name: "Chronology calibration" });
}

async function fillCorrection(app: Awaited<ReturnType<typeof renderApp>>, panel: HTMLElement) {
  await app.user.selectOptions(within(panel).getByLabelText(/Capture sequence/), "sequence_sony");
  await app.user.clear(within(panel).getByLabelText(/Shift by/));
  await app.user.type(within(panel).getByLabelText(/Shift by/), "60");
  await app.user.type(within(panel).getByLabelText("Why"), "Clock never left home time");
  await app.user.click(within(panel).getByRole("button", { name: "Preview impact" }));
  return screen.findByRole("dialog", { name: "Confirm chronology change" });
}

describe("CUJ 10 — correcting a camera clock", () => {
  it("loads the chronology the backend declared", async () => {
    const harness = miscalibrated().build();
    const app = await renderApp(harness);

    const panel = await enterChronology(app);
    expect(within(panel).getByText("3 clips")).toBeVisible();
    expect(within(panel).getByText("2 capture sequences")).toBeVisible();
    expect(within(panel).getByText("No clock corrections yet.")).toBeVisible();
  });

  it("previews before it writes, and writes only on confirmation", async () => {
    const harness = miscalibrated().build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);
    harness.clearLog();

    const confirm = await fillCorrection(app, panel);

    // Previewed, and nothing written yet.
    expect(harness.commandNames()).toContain("preview_clock_epoch");
    expect(harness.commandNames()).not.toContain("set_clock_epoch");
    expect(within(confirm).getByText("No contradictions.")).toBeVisible();

    await app.user.click(within(confirm).getByRole("button", { name: "Apply" }));

    await waitFor(() => expect(harness.calls("set_clock_epoch")).toHaveLength(1));
    // The safety ordering, in the order it must happen. Consecutive repeats are
    // collapsed: the panel and the shell each refresh after the write, and how
    // many reads that costs is not the contract.
    const ordering = harness
      .commandNames()
      .filter((command) =>
        ["preview_clock_epoch", "capture_queue_position", "set_clock_epoch", "chronology_overview", "restore_queue_position"].includes(
          command,
        ),
      )
      .filter((command, index, all) => command !== all[index - 1]);
    expect(ordering).toEqual([
      "preview_clock_epoch",
      "capture_queue_position",
      "set_clock_epoch",
      "chronology_overview",
      "restore_queue_position",
    ]);
  });

  it("sends the correction the user described", async () => {
    const harness = miscalibrated().build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    const confirm = await fillCorrection(app, panel);
    await app.user.click(within(confirm).getByRole("button", { name: "Apply" }));

    await waitFor(() => expect(harness.calls("set_clock_epoch")).toHaveLength(1));
    expect(harness.calls("set_clock_epoch")[0]).toMatchObject({
      projectId: "project_hokkaido",
      epoch: {
        captureSequenceId: "sequence_sony",
        offsetSeconds: 3_600,
        label: "Clock never left home time",
      },
    });
  });

  it("refreshes to the declared ordering and records the correction", async () => {
    const harness = miscalibrated().build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    const confirm = await fillCorrection(app, panel);
    await app.user.click(within(confirm).getByRole("button", { name: "Apply" }));

    await waitFor(() =>
      expect(harness.state.chronology.overview.assets.map((asset) => asset.assetId)).toEqual([
        dji[0],
        sony[0],
        sony[1],
      ]),
    );
    expect(await within(panel).findByText(/Clock never left home time — \+1h 0m/)).toBeVisible();
  });

  it("returns the user to review on the clip they were on", async () => {
    const harness = miscalibrated().build();
    const app = await renderApp(harness);

    await app.user.click(screen.getByText("SONY_002.MP4"));
    await waitFor(() => expect(selectedRow()?.textContent).toContain("SONY_002.MP4"));

    const panel = await enterChronology(app);
    const confirm = await fillCorrection(app, panel);
    await app.user.click(within(confirm).getByRole("button", { name: "Apply" }));

    await waitFor(() => expect(harness.calls("restore_queue_position")).toHaveLength(1));
    expect(harness.calls("capture_queue_position")[0]).toEqual({
      projectId: "project_hokkaido",
      currentAssetId: sony[1],
    });

    await app.user.click(screen.getByRole("button", { name: "Review" }));
    expect(selectedRow()?.textContent).toContain("SONY_002.MP4");
  });

  it("follows the backend when the reorder resumes somewhere else", async () => {
    const harness = miscalibrated().resumesAt(dji[0]).build();
    const app = await renderApp(harness);

    const panel = await enterChronology(app);
    const confirm = await fillCorrection(app, panel);
    await app.user.click(within(confirm).getByRole("button", { name: "Apply" }));

    await waitFor(() => expect(harness.calls("restore_queue_position")).toHaveLength(1));
    await app.user.click(screen.getByRole("button", { name: "Review" }));
    await waitFor(() => expect(selectedRow()?.textContent).toContain("DJI_001.MP4"));
  });

  it("removes a correction the user no longer wants", async () => {
    const harness = miscalibrated().build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    const confirm = await fillCorrection(app, panel);
    await app.user.click(within(confirm).getByRole("button", { name: "Apply" }));
    await within(panel).findByText(/Clock never left home time/);

    await app.user.click(within(panel).getByRole("button", { name: "Remove" }));

    await waitFor(() => expect(harness.state.chronology.overview.epochs).toHaveLength(0));
    expect(await within(panel).findByText("No clock corrections yet.")).toBeVisible();
  });
});

describe("CUJ 11 — a correction that would contradict the evidence", () => {
  it("shows the contradiction, disables Apply, and issues no write", async () => {
    const harness = miscalibrated()
      .contradictionOnPreview(
        "sequence_sony",
        buildContradiction({
          strength: "hard",
          assetIds: [sony[0], dji[0]],
          message: "SONY_001 would land after DJI_001, which its GPS fix contradicts",
          evidence: ["gps fix at 00:12", "camera clock at 01:10"],
        }),
      )
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);
    harness.clearLog();

    const confirm = await fillCorrection(app, panel);

    expect(within(confirm).getByText(/This would create 1 contradiction/)).toBeVisible();
    expect(within(confirm).getByText(/its GPS fix contradicts/)).toBeVisible();
    expect(within(confirm).getByRole("button", { name: "Apply" })).toBeDisabled();

    // The one assertion the command log exists for: nothing was written.
    expect(harness.commandNames()).not.toContain("set_clock_epoch");
    expect(harness.commandNames()).not.toContain("capture_queue_position");
    expect(harness.state.chronology.overview.epochs).toHaveLength(0);
  });

  it("lets the user back out without writing", async () => {
    const harness = miscalibrated().build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    const confirm = await fillCorrection(app, panel);
    await app.user.click(within(confirm).getByRole("button", { name: "Cancel" }));

    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Confirm chronology change" })).toBeNull());
    expect(harness.commandNames()).not.toContain("set_clock_epoch");
  });

  it("names the contradictions already blocking the project", async () => {
    const harness = miscalibrated()
      .chronology({
        order: [sony[0], sony[1], dji[0]],
        sequences: [{ id: "sequence_sony", cameraKey: "Sony A7C II" }],
        contradictions: [
          buildContradiction({ strength: "hard", message: "Two clips claim the same instant" }),
          buildContradiction({ strength: "soft", message: "A gap longer than the trip" }),
        ],
      })
      .build();
    const app = await renderApp(harness);

    const panel = await enterChronology(app);
    // Only the hard one blocks, and it says which evidence disagrees.
    expect(within(panel).getByText("1 contradiction(s) block changes")).toBeVisible();
    expect(within(panel).getByText("Two clips claim the same instant")).toBeVisible();
    expect(within(panel).queryByText("A gap longer than the trip")).toBeNull();
  });
});

describe("diagnostics", () => {
  it("shows what needs attention and stops showing it once acknowledged", async () => {
    const harness = miscalibrated()
      .diagnostic(
        buildDiagnostic({
          id: "diagnostic_offline_travel",
          severity: "error",
          message: "3 clips on /Volumes/Travel are unreachable",
          volumeHint: "/Volumes/Travel",
          assetIds: [sony[0]],
        }),
      )
      .offlineOriginal(sony[0])
      .build();
    const app = await renderApp(harness);

    const panel = await enterChronology(app);
    expect(within(panel).getByText(/Needs attention — 1 error/)).toBeVisible();
    expect(within(panel).getByText(/unreachable/)).toBeVisible();

    await app.user.click(within(panel).getByRole("button", { name: "Acknowledge" }));

    await waitFor(() => expect(harness.state.diagnostics[0].acknowledgedAt).toBe("2026-08-09T12:00:00.000Z"));
    await waitFor(() => expect(within(panel).queryByText(/Needs attention/)).toBeNull());
  });

  it("offers Locate… for offline media and repairs what the folder contains", async () => {
    const harness = miscalibrated()
      .diagnostic(
        buildDiagnostic({
          id: "diagnostic_offline_travel",
          severity: "error",
          message: "Clips on /Volumes/Travel are unreachable",
          volumeHint: "/Volumes/Travel",
          assetIds: [sony[0], sony[1]],
        }),
      )
      .offlineOriginal(sony[0], sony[1])
      .relinkableAt("/Volumes/Travel2", sony[0], sony[1])
      .dialogAnswers("/Volumes/Travel2")
      .build();
    const app = await renderApp(harness);

    const panel = await enterChronology(app);
    await app.user.click(within(panel).getByRole("button", { name: "Locate…" }));

    await waitFor(() => expect(harness.state.assets[sony[0]].originalAvailable).toBe(true));
    expect(harness.state.assets[sony[1]].originalAvailable).toBe(true);
    expect(await within(panel).findByText(/Reconnected 2 clip\(s\)/)).toBeVisible();
  });
});
