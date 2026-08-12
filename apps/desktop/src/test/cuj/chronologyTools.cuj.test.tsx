/**
 * The chronology desk's correction tools.
 *
 * `chronology.cuj.test.tsx` covers entry, return, and the clock/placement
 * paths. This file covers the rest of the desk: card merges and splits, undoing
 * a persisted correction, moving a trip-phase boundary from the canvas, walking
 * the worklist, and what the desk does when the read itself fails.
 *
 * As everywhere in this tier, solver results are *declared*. What is under test
 * is the frontend's safety contract — nothing is written without a preview the
 * user confirmed, and one confirmation is one write.
 */

import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type {
  CaptureSequenceMergePreview,
  CaptureSequenceSplitPreview,
  ChronologyExplanation,
  ChronologyImpact,
  ClockEpoch,
  TripPhase,
} from "../../chronologyTypes";
import { buildContradiction, desktopScenario, renderApp } from "../harness";

const sony = ["asset_sony_a7c_ii_001", "asset_sony_a7c_ii_002"];
const dji = ["asset_dji_pocket_3_001"];

const sequences = [
  {
    id: "sequence_sony",
    cameraKey: "Sony A7C II",
    label: "Sony travel card",
    volumeHint: "/Volumes/Travel",
    assetIds: sony,
  },
  { id: "sequence_dji", cameraKey: "DJI Pocket 3", label: "DJI day card", assetIds: dji },
];

const movesOneClip: ChronologyImpact = {
  changes: [{
    assetId: dji[0],
    before: "2026-02-08T00:20:00.000Z",
    after: "2026-02-08T00:05:00.000Z",
    beforePosition: 2,
    afterPosition: 0,
  }],
  contradictions: [],
  blocked: false,
};

const persistedEpoch: ClockEpoch = {
  id: "epoch_persisted_sony",
  captureSequenceId: "sequence_sony",
  startCounter: 0,
  endCounter: null,
  offsetSeconds: -5_400,
  confidence: 1,
  evidence: ["user_set"],
  label: "Home time until Osaka",
};

const tokyo: TripPhase = {
  id: "phase_tokyo",
  label: "Tokyo",
  kind: "location",
  startAnchorAssetId: sony[0],
  endAnchorAssetId: sony[1],
  startTripTime: "2026-02-08T00:10:00.000Z",
  endTripTime: "2026-02-08T00:15:00.000Z",
  timezone: "Asia/Tokyo",
  originTimezone: null,
  destinationTimezone: null,
  displayMode: "local",
};

const mergePreview: CaptureSequenceMergePreview = {
  fromSequenceId: "sequence_dji",
  intoSequenceId: "sequence_sony",
  assetIds: dji,
  epochIds: [],
  factsToken: "facts-merge-dji-into-sony",
  impact: movesOneClip,
};

const splitPreview: CaptureSequenceSplitPreview = {
  sourceSequenceId: "sequence_sony",
  atCounter: 1,
  createdSequence: {
    id: "sequence_sony_second_card",
    cameraKey: "Sony A7C II",
    label: "Sony travel card · new card",
    volumeHint: "/Volumes/Travel",
    filenamePrefix: "SONY_",
  },
  assetIds: [sony[1]],
  factsToken: "facts-split-sony",
  impact: movesOneClip,
};

function trip() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 2, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" })
    .camera("DJI Pocket 3", { clips: 1, withProxy: true, startAt: "2026-02-08T00:20:00.000Z" })
    .chronology({ order: [sony[0], sony[1], dji[0]], sequences });
}

async function enterChronology(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(screen.getByRole("button", { name: "Chronology" }));
  return screen.findByRole("region", { name: "Chronology calibration" });
}

function impactDock() {
  return screen.findByRole("dialog", { name: "Chronology impact preview" });
}

describe("Card grouping corrections", () => {
  it("previews a whole-card merge and writes it only once confirmed", async () => {
    const harness = trip()
      .captureSequenceMerge(mergePreview, { order: [dji[0], sony[0], sony[1]], sequences })
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);
    harness.clearLog();

    // The merge tool moves the *focused* clip's card, so focus a DJI clip first.
    await app.user.click(within(panel).getByRole("button", { name: /DJI_001\.MP4, clip 3/ }));
    await app.user.click(within(panel).getByRole("button", { name: "Card" }));
    await app.user.selectOptions(
      within(panel).getByLabelText(/Merge this whole card into/),
      "sequence_sony",
    );
    await app.user.click(within(panel).getByRole("button", { name: "Preview whole-card merge" }));

    const dock = await impactDock();
    expect(within(dock).getByRole("heading", { name: /Merge into Sony travel card/ })).toBeVisible();
    expect(within(dock).getByText("#3 → #1")).toBeVisible();
    expect(harness.calls("apply_capture_sequence_merge")).toHaveLength(0);

    await app.user.click(within(dock).getByRole("button", { name: "Apply change" }));

    await waitFor(() => expect(harness.calls("apply_capture_sequence_merge")).toHaveLength(1));
    expect(harness.calls("apply_capture_sequence_merge")[0]).toEqual({
      projectId: "project_hokkaido",
      preview: mergePreview,
    });
    expect(harness.state.chronology.overview.assets.map((asset) => asset.assetId))
      .toEqual([dji[0], sony[0], sony[1]]);
  });

  it("previews the solver's own suggested card boundary and applies that exact split", async () => {
    const harness = trip()
      .chronology({
        order: [sony[0], sony[1], dji[0]],
        sequences,
        proposedSplits: [{
          captureSequenceId: "sequence_sony",
          atCounter: 1,
          reason: "Filename counter restarts at 1 mid-card",
          affectedAssetIds: [sony[1]],
        }],
      })
      .captureSequenceSplit(splitPreview, { order: [dji[0], sony[0], sony[1]], sequences })
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);
    harness.clearLog();

    // Answered from the boundary question itself rather than the Card tab: the
    // suggestion is a question with two answers, and "yes" is the one that
    // still has to go through preview-then-confirm like every other write.
    await app.user.click(within(panel).getByRole("button", { name: /Yes — new card/ }));

    const dock = await impactDock();
    expect(within(dock).getByRole("heading", { name: /Create Sony travel card · new card/ }))
      .toBeVisible();
    await app.user.click(within(dock).getByRole("button", { name: "Apply change" }));

    await waitFor(() => expect(harness.calls("apply_capture_sequence_split")).toHaveLength(1));
    expect(harness.calls("apply_capture_sequence_split")[0]).toEqual({
      projectId: "project_hokkaido",
      preview: splitPreview,
    });
    // The suggestion is a proposal, never a write of its own.
    expect(harness.calls("split_capture_sequence")).toHaveLength(0);
  });
});

describe("Undoing a persisted correction", () => {
  it("shows what removing an existing clock correction would move, then removes it", async () => {
    const harness = trip()
      .chronology({ order: [sony[0], sony[1], dji[0]], sequences, epochs: [persistedEpoch] })
      .impactOnClearClockEpochPreview(persistedEpoch.id, movesOneClip)
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);
    harness.clearLog();

    await app.user.click(within(panel).getByRole("button", { name: "Clock" }));
    await app.user.click(within(panel).getByRole("button", { name: /Home time until Osaka/ }));

    const dock = await impactDock();
    expect(within(dock).getByRole("heading", { name: "Remove Home time until Osaka" })).toBeVisible();
    expect(dock.querySelector(".chronology-impact-summary p")).toHaveTextContent("1 clip moves");
    expect(harness.calls("clear_clock_epoch")).toHaveLength(0);

    await app.user.click(within(dock).getByRole("button", { name: "Apply change" }));

    await waitFor(() => expect(harness.calls("clear_clock_epoch")).toHaveLength(1));
    expect(harness.calls("clear_clock_epoch")[0]).toEqual({
      projectId: "project_hokkaido",
      preview: {
        epochId: persistedEpoch.id,
        factsToken: `facts-clear-${persistedEpoch.id}`,
        impact: movesOneClip,
      },
    });
  });

  it("abandons a pending preview on Escape without writing anything", async () => {
    const harness = trip()
      .chronology({ order: [sony[0], sony[1], dji[0]], sequences, epochs: [persistedEpoch] })
      .impactOnClearClockEpochPreview(persistedEpoch.id, movesOneClip)
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    await app.user.click(within(panel).getByRole("button", { name: "Clock" }));
    await app.user.click(within(panel).getByRole("button", { name: /Home time until Osaka/ }));
    await impactDock();

    await app.user.keyboard("{Escape}");

    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "Chronology impact preview" })).toBeNull());
    expect(harness.calls("clear_clock_epoch")).toHaveLength(0);
    // Escape closed the preview, not the desk.
    expect(screen.getByRole("region", { name: "Chronology calibration" })).toBeVisible();
  });
});

describe("Trip phases on the canvas", () => {
  it("moves a phase boundary from the canvas and writes the clip it was dropped on", async () => {
    const harness = trip()
      .chronology({ order: [sony[0], sony[1], dji[0]], sequences, phases: [tokyo] })
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);
    harness.clearLog();

    fireEvent.keyDown(within(panel).getByRole("button", { name: "Move end of Tokyo" }), {
      key: "ArrowRight",
    });

    const dialog = await screen.findByRole("dialog", { name: /Edit Tokyo/ });
    expect(within(dialog).getByLabelText("Selected clip range")).toHaveTextContent("3 clips");
    await app.user.click(within(dialog).getByRole("button", { name: "Save phase" }));

    await waitFor(() => expect(harness.calls("set_trip_phase")).toHaveLength(1));
    const written = harness.calls("set_trip_phase")[0].phase as TripPhase;
    expect(written.id).toBe("phase_tokyo");
    expect(written.startAnchorAssetId).toBe(sony[0]);
    expect(written.endAnchorAssetId).toBe(dji[0]);
  });

  it("closes the boundary dialog without writing when it is cancelled", async () => {
    const harness = trip()
      .chronology({ order: [sony[0], sony[1], dji[0]], sequences, phases: [tokyo] })
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);
    harness.clearLog();

    await app.user.click(within(panel).getByRole("button", { name: "Edit Tokyo" }));
    const dialog = await screen.findByRole("dialog", { name: /Edit Tokyo/ });
    await app.user.click(within(dialog).getByRole("button", { name: "Close context dialog" }));

    await waitFor(() => expect(screen.queryByRole("dialog", { name: /Edit Tokyo/ })).toBeNull());
    expect(harness.calls("set_trip_phase")).toHaveLength(0);
  });
});

describe("Working the chronology worklist", () => {
  it("steps through every problem and focuses the clip each one is about", async () => {
    const harness = trip()
      .chronology({
        order: [sony[0], sony[1], dji[0]],
        sequences,
        contradictions: [
          buildContradiction({ assetIds: [sony[0]], message: "SONY_001 conflicts with a pinned time" }),
          buildContradiction({
            strength: "soft",
            assetIds: [dji[0]],
            message: "DJI_001 may sit before the ferry boarding sequence",
          }),
        ],
      })
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);
    const inspector = within(panel).getByRole("complementary", { name: "Chronology inspector" });

    expect(within(panel).getByText("1/2")).toBeVisible();

    await app.user.click(within(panel).getByRole("button", { name: "Next chronology problem" }));
    expect(within(panel).getByText("2/2")).toBeVisible();
    expect(within(inspector).getByRole("heading", { name: "DJI_001.MP4" })).toBeVisible();

    // The worklist wraps rather than dead-ending on its last item.
    await app.user.click(within(panel).getByRole("button", { name: "Next chronology problem" }));
    expect(within(panel).getByText("1/2")).toBeVisible();
    expect(within(inspector).getByRole("heading", { name: "SONY_001.MP4" })).toBeVisible();

    await app.user.click(within(panel).getByRole("button", { name: "Previous chronology problem" }));
    expect(within(panel).getByText("2/2")).toBeVisible();
  });

  it("counts clips with no usable time as one thing to look at", async () => {
    const overview = trip().build().state.chronology.overview;
    const harness = trip()
      .chronology({
        ...overview,
        assets: overview.assets.map((asset, index) =>
          index === 2 ? { ...asset, tripTime: null, displayTime: null } : asset),
        unplacedAssets: 1,
      })
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    const rail = within(panel).getByRole("complementary", { name: "Chronology problems" });
    expect(within(rail).getByText("1 clips have no usable time")).toBeVisible();
    expect(within(panel).getByText("1 unplaced")).toBeVisible();
  });

  it("says the order looks trustworthy when the solver reports nothing to fix", async () => {
    const app = await renderApp(trip().build());
    const panel = await enterChronology(app);

    expect(within(panel).getByText("Order looks trustworthy")).toBeVisible();
    expect(within(panel).getByRole("heading", { name: "All in order" })).toBeVisible();
    expect(within(panel).queryByLabelText("Problem navigation")).toBeNull();
  });
});

describe("Why a clip sits where it does", () => {
  it("lists the corrections the solver actually applied to the focused clip", async () => {
    const explanation: ChronologyExplanation = {
      assetId: sony[0],
      rawTime: "2026-02-08T00:10:00.000Z",
      tripTime: "2026-02-08T09:10:00.000Z",
      displayTime: {
        primary: "2026-02-08T09:10:00+09:00",
        secondary: null,
        timezone: "Asia/Tokyo",
        phaseId: "phase_tokyo",
      },
      confidence: 0.82,
      uncertaintySeconds: 120,
      appliedCorrections: [
        { type: "captureSequence", sequenceId: "sequence_sony", label: "Sony travel card" },
        {
          type: "clockEpoch",
          epochId: persistedEpoch.id,
          offsetSeconds: -5_400,
          label: "Home time until Osaka",
        },
        { type: "tripPhase", phaseId: tokyo.id, timezone: "Asia/Tokyo", label: "Tokyo" },
      ],
      contradictions: [],
    };
    const harness = trip().build();
    harness.state.chronology.explanations[sony[0]] = explanation;
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    const inspector = within(panel).getByRole("complementary", { name: "Chronology inspector" });
    await within(inspector).findByText("Grouped with Sony travel card");
    expect(within(inspector).getByText(/Clock shifted −1h 30m · Home time until Osaka/)).toBeVisible();
    expect(within(inspector).getByText("Displayed in Asia/Tokyo · Tokyo")).toBeVisible();
    // The confidence meter reads the live overview, not the explanation, so it
    // cannot drift from the order actually on screen.
    expect(inspector.querySelector(".chronology-confidence")).toHaveTextContent("90% confidence");
  });

  it("says plainly when a clip carries no explicit corrections", async () => {
    const harness = trip().build();
    harness.state.chronology.explanations[sony[0]] = {
      assetId: sony[0],
      rawTime: "2026-02-08T00:10:00.000Z",
      tripTime: "2026-02-08T00:10:00.000Z",
      displayTime: null,
      confidence: 1,
      uncertaintySeconds: 0,
      appliedCorrections: [],
      contradictions: [],
    };
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    const inspector = within(panel).getByRole("complementary", { name: "Chronology inspector" });
    await within(inspector).findByText("No explicit corrections.");
    expect(inspector.querySelector(".chronology-confidence")).toHaveTextContent("Exact");
  });
});

describe("When the chronology read fails", () => {
  it("keeps Review usable, explains the failure on the desk, and retries on demand", async () => {
    const harness = trip().build();
    harness.faults.always("chronology_overview", {
      kind: "reject",
      message: "the project catalog is locked by another process",
    });
    const app = await renderApp(harness);

    // Review does not depend on the chronology read.
    expect(screen.getByText("SONY_001.MP4")).toBeVisible();

    await app.user.click(screen.getByRole("button", { name: "Chronology" }));
    const failed = await screen.findByRole("heading", { name: "Chronology could not be loaded" });
    expect(failed).toBeVisible();
    expect(screen.getByRole("alert")).toHaveTextContent("locked by another process");

    harness.faults.clear();
    harness.clearLog();
    await app.user.click(screen.getByRole("button", { name: "Try again" }));

    await screen.findByRole("region", { name: "Chronology calibration" });
    expect(harness.calls("chronology_overview").length).toBeGreaterThan(0);
  });
});
