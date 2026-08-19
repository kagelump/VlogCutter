import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { ClockEpoch } from "../../chronologyTypes";
import { desktopScenario, renderApp } from "../harness";

const right = "asset_sony_001";
const wrong = ["asset_sony_wrong_001", "asset_sony_wrong_002"];
const epoch: ClockEpoch = {
  id: "epoch_wrong_card",
  captureSequenceId: "card_wrong_a",
  startCounter: 1,
  endCounter: null,
  offsetSeconds: 45,
  confidence: 1,
  evidence: ["user_set"],
  label: "Clock correction stays with card A",
};

function cameraSplitTrip() {
  const harness = desktopScenario({ now: "2026-08-19T12:00:00.000Z" })
    .project("Split camera identity", { id: "project_camera_merge" })
    .camera("Sony", { clips: 1, withProxy: true })
    .camera("Sony wrong", { clips: 2, withProxy: true })
    .chronology({
      order: [right, ...wrong],
      sequences: [
        { id: "card_right_a", cameraKey: "Sony", label: "Sony card A", assetIds: [right] },
        { id: "card_right_b", cameraKey: "Sony", label: "Sony card B", assetIds: [] },
        { id: "card_wrong_a", cameraKey: "Sony wrong", label: "Wrong card A", assetIds: [wrong[0]] },
        { id: "card_wrong_b", cameraKey: "Sony wrong", label: "Wrong card B", assetIds: [wrong[1]] },
      ],
      epochs: [epoch],
    })
    .build();
  harness.state.imports.cameraMappings.project_camera_merge = [{
    id: "mapping_wrong",
    folderHint: "PRIVATE/M4ROOT",
    filenamePrefix: "C",
    exifHint: "SONY ILCE",
    camera: "Sony wrong",
    createdAt: "2026-08-19T00:00:00Z",
    updatedAt: "2026-08-19T00:00:00Z",
  }];
  return harness;
}

describe("camera identity merge", () => {
  it("previews counts, preserves cards and epochs, and refreshes Review and Chronology together", async () => {
    const harness = cameraSplitTrip();
    const app = await renderApp(harness);

    await app.user.click(screen.getByRole("button", { name: "Camera actions for Sony wrong" }));
    await app.user.click(screen.getByRole("button", { name: "Merge with…" }));
    const dialog = await screen.findByRole("dialog", { name: "Merge camera lanes" });
    await app.user.selectOptions(within(dialog).getByLabelText("Merge with…"), "Sony");

    await waitFor(() => expect(within(dialog).getByText("Clips moving").nextSibling).toHaveTextContent("2"));
    expect(within(dialog).getByText("Cards moving").nextSibling).toHaveTextContent("2");
    expect(within(dialog).getByText("Cards already there").nextSibling).toHaveTextContent("2");
    expect(within(dialog).getByText(/All 4 cards stay separate/)).toBeVisible();

    await app.user.click(within(dialog).getByRole("button", { name: "Merge camera" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Merge camera lanes" })).toBeNull());
    expect(screen.queryByRole("button", { name: "Camera actions for Sony wrong" })).toBeNull();
    expect(screen.getAllByRole("button", { name: "Camera actions for Sony" })).toHaveLength(1);

    expect(harness.state.chronology.overview.sequences.map((sequence) => sequence.id)).toEqual([
      "card_right_a", "card_right_b", "card_wrong_a", "card_wrong_b",
    ]);
    expect(harness.state.chronology.overview.sequences.every((sequence) => sequence.cameraKey === "Sony")).toBe(true);
    expect(harness.state.chronology.overview.epochs[0].captureSequenceId).toBe("card_wrong_a");
    expect(harness.state.imports.cameraMappings.project_camera_merge[0].camera).toBe("Sony");

    await app.user.click(screen.getByRole("button", { name: "Chronology" }));
    const chronology = await screen.findByRole("region", { name: "Chronology calibration" });
    const summary = within(chronology).getByLabelText("Chronology summary");
    expect(within(summary).getByText("1 camera")).toBeVisible();
    expect(within(summary).getByText("4 cards")).toBeVisible();
  });

  it("Escape closes only the merge dialog and restores the lane action trigger", async () => {
    const harness = cameraSplitTrip();
    const app = await renderApp(harness);
    await app.user.click(screen.getByRole("button", { name: "Camera actions for Sony wrong" }));
    await app.user.click(screen.getByRole("button", { name: "Merge with…" }));
    await screen.findByRole("dialog", { name: "Merge camera lanes" });

    await app.user.keyboard("{Escape}");

    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Merge camera lanes" })).toBeNull());
    const trigger = screen.getByRole("button", { name: "Camera actions for Sony wrong" });
    expect(trigger).toHaveFocus();
    expect(screen.getByRole("button", { name: "Review" })).toHaveAttribute("aria-pressed", "true");
    expect(harness.calls("merge_camera")).toHaveLength(0);
  });

  it("offers destinations represented only by removed media or an empty chronology card", async () => {
    const harness = desktopScenario({ now: "2026-08-19T12:00:00.000Z" })
      .project("Hidden camera destinations", { id: "project_hidden_destinations" })
      .camera("Source", { clips: 1, withProxy: true })
      .camera("Archived", { clips: 1, withProxy: true })
      .removed("asset_archived_001")
      .chronology({
        order: ["asset_source_001"],
        sequences: [
          { id: "card_source", cameraKey: "Source", label: "Source card", assetIds: ["asset_source_001"] },
          { id: "card_empty", cameraKey: "Empty body", label: "Empty card", assetIds: [] },
        ],
      })
      .build();
    const app = await renderApp(harness);

    expect(screen.queryByRole("button", { name: "Camera actions for Archived" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Camera actions for Empty body" })).toBeNull();
    await app.user.click(screen.getByRole("button", { name: "Camera actions for Source" }));
    await app.user.click(screen.getByRole("button", { name: "Merge with…" }));
    const destination = within(await screen.findByRole("dialog", { name: "Merge camera lanes" }))
      .getByLabelText("Merge with…");
    expect(within(destination).getByRole("option", { name: "Archived" })).toBeVisible();
    expect(within(destination).getByRole("option", { name: "Empty body" })).toBeVisible();
  });
});
