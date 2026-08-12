/**
 * The desktop chronology CUJ.
 *
 * Solver results are scenario declarations. These tests exercise only the
 * frontend contract: problems are discoverable from Review, entry and return
 * preserve the user's place, previews precede explicit writes, and duplicate
 * intent cannot produce duplicate operations.
 */

import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type {
  CaptureSequenceReassignmentPreview,
  ChronologyImpact,
  ClockEpoch,
  VisualPlacementPreview,
} from "../../chronologyTypes";
import {
  buildContradiction,
  buildDiagnostic,
  desktopScenario,
  renderApp,
  selectedRow,
} from "../harness";

const sony = ["asset_sony_a7c_ii_001", "asset_sony_a7c_ii_002"];
const dji = ["asset_dji_pocket_3_001"];

const clockImpact: ChronologyImpact = {
  changes: [
    {
      assetId: sony[0],
      before: "2026-02-08T00:10:00.000Z",
      after: "2026-02-08T01:10:00.000Z",
      beforePosition: 0,
      afterPosition: 1,
    },
    {
      assetId: sony[1],
      before: "2026-02-08T00:15:00.000Z",
      after: "2026-02-08T01:15:00.000Z",
      beforePosition: 1,
      afterPosition: 2,
    },
  ],
  contradictions: [],
  blocked: false,
};

const sequences = [
  {
    id: "sequence_sony",
    cameraKey: "Sony A7C II",
    label: "Sony travel card",
    volumeHint: "/Volumes/Travel",
    assetIds: sony,
  },
  {
    id: "sequence_dji",
    cameraKey: "DJI Pocket 3",
    label: "DJI day card",
    assetIds: dji,
  },
];

const clockEpoch: ClockEpoch = {
  id: "epoch_preview_sony",
  captureSequenceId: "sequence_sony",
  startCounter: 0,
  endCounter: null,
  offsetSeconds: 3_600,
  confidence: 1,
  evidence: ["user_set"],
  label: "Clock never left home time",
};

const visualPlacementPreview: VisualPlacementPreview = {
  epoch: {
    ...clockEpoch,
    id: "epoch_visual_sony",
    offsetSeconds: 600,
    label: "Placed visually in Chronology",
  },
  factsToken: "facts-visual-sony",
  impact: clockImpact,
};

const singleReassignment: CaptureSequenceReassignmentPreview = {
  fromSequenceId: "sequence_sony",
  intoSequenceId: "sequence_dji",
  assetIds: [sony[0]],
  factsToken: "facts-reassign-single",
  impact: { changes: [clockImpact.changes[0]], contradictions: [], blocked: false },
};

const followingReassignment: CaptureSequenceReassignmentPreview = {
  fromSequenceId: "sequence_sony",
  intoSequenceId: "sequence_dji",
  assetIds: sony,
  factsToken: "facts-reassign-following",
  impact: clockImpact,
};

function chronologyTrip() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 2, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" })
    .camera("DJI Pocket 3", { clips: 1, withProxy: true, startAt: "2026-02-08T00:20:00.000Z" })
    .chronology({ order: [sony[0], sony[1], dji[0]], sequences })
    .impactOnClockPreview("sequence_sony", { epoch: clockEpoch, factsToken: "facts-clock-sony", impact: clockImpact })
    .whenClockEpochApplied("sequence_sony")
    .thenChronology({ order: [dji[0], sony[0], sony[1]], sequences });
}

function chronologyWithProblem() {
  return chronologyTrip().chronology({
    order: [sony[0], sony[1], dji[0]],
    sequences,
    contradictions: [
      buildContradiction({
        strength: "soft",
        assetIds: [dji[0]],
        message: "DJI_001 may sit before the ferry boarding sequence",
      }),
    ],
  });
}

/**
 * One camera, two cards: the DJI counter restarts, which is what an SD swap
 * looks like — and also what a camera that simply rolls its counter over looks
 * like. The desk cannot tell the difference, which is why it asks.
 */
function chronologyWithCardBoundary() {
  const proposal = {
    captureSequenceId: "sequence_dji",
    atCounter: 1,
    reason:
      "the filename counter restarts at 1 partway through this sequence, which is what a second card looks like",
    affectedAssetIds: dji,
  };
  return chronologyTrip()
    .chronology({ order: [sony[0], sony[1], dji[0]], sequences, proposedSplits: [proposal] })
    .captureSequenceSplit(
      {
        sourceSequenceId: "sequence_dji",
        atCounter: 1,
        createdSequence: {
          id: "sequence_dji_card_two",
          cameraKey: "DJI Pocket 3",
          label: "DJI day card · new card",
          volumeHint: "",
          filenamePrefix: "DJI",
        },
        assetIds: dji,
        factsToken: "facts-split-dji",
        impact: { changes: [], contradictions: [], blocked: false },
      },
      { order: [sony[0], sony[1], dji[0]], sequences },
    );
}

async function enterChronology(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(screen.getByRole("button", { name: "Chronology" }));
  return screen.findByRole("region", { name: "Chronology calibration" });
}

async function previewClockCorrection(
  app: Awaited<ReturnType<typeof renderApp>>,
  panel: HTMLElement,
) {
  await app.user.click(within(panel).getByRole("button", { name: "Clock" }));
  await app.user.selectOptions(within(panel).getByLabelText(/Card \/ capture sequence/), "sequence_sony");
  await app.user.clear(within(panel).getByLabelText("Shift by minutes"));
  await app.user.type(within(panel).getByLabelText("Shift by minutes"), "60");
  await app.user.type(within(panel).getByLabelText("Reason"), "Clock never left home time");
  await app.user.click(within(panel).getByRole("button", { name: "Preview correction" }));
  return screen.findByRole("dialog", { name: "Chronology impact preview" });
}

describe("Review entry and stable return", () => {
  it("keeps Review free of chronology chrome when there are no problems", async () => {
    await renderApp(chronologyTrip().build());
    expect(screen.queryByRole("button", { name: /chronology problem/i })).toBeNull();
  });

  it("shows a problem chip and deep-links to the first affected clip", async () => {
    const app = await renderApp(chronologyWithProblem().build());
    const chip = await screen.findByRole("button", { name: /1 chronology problem.*Fix/i });

    await app.user.click(chip);

    const panel = await screen.findByRole("region", { name: "Chronology calibration" });
    const rail = within(panel).getByRole("complementary", { name: "Chronology problems" });
    expect(within(rail).getByText(/DJI_001 may sit before/)).toBeVisible();
    const inspector = within(panel).getByRole("complementary", { name: "Chronology inspector" });
    expect(within(inspector).getByRole("heading", { name: "DJI_001.MP4" })).toBeVisible();
  });

  it("opens with Shift+T and restores selection, ribbon zoom/scroll, and queue scroll", async () => {
    const app = await renderApp(chronologyTrip().build());
    await app.user.click(screen.getByText("SONY_002.MP4"));
    await app.user.click(screen.getByLabelText("Zoom story ribbon in"));

    const reviewRibbon = document.querySelector<HTMLDivElement>(".story-scroll")!;
    const reviewQueue = screen.getByRole("listbox", { name: "Clips" }) as HTMLDivElement;
    reviewRibbon.scrollLeft = 173;
    reviewQueue.scrollTop = 61;

    await app.user.keyboard("{Shift>}T{/Shift}");

    const panel = await screen.findByRole("region", { name: "Chronology calibration" });
    expect(within(panel).getByRole("button", { name: "Back to SONY_002.MP4" })).toBeVisible();
    const expandedScroll = panel.querySelector<HTMLDivElement>(".chronology-canvas-scroll")!;
    const expandedCanvas = panel.querySelector<HTMLElement>(".chronology-canvas")!;
    await waitFor(() => expect(expandedScroll.scrollLeft).toBe(173));
    expect(expandedCanvas.style.getPropertyValue("--chronology-column")).toBe("80px");
    await app.user.click(within(panel).getByRole("button", { name: "Back to SONY_002.MP4" }));

    await waitFor(() => expect(selectedRow()?.textContent).toContain("SONY_002.MP4"));
    await waitFor(() => {
      expect((document.querySelector(".story-scroll") as HTMLDivElement).scrollLeft).toBe(173);
      expect((screen.getByRole("listbox", { name: "Clips" }) as HTMLDivElement).scrollTop).toBe(61);
    });
    expect(screen.getByLabelText("Story ribbon zoom level")).toHaveValue("80");
  });
});

describe("Chronology desk", () => {
  it("presents the problem rail and one visual lane per declared capture sequence", async () => {
    const app = await renderApp(chronologyWithProblem().build());
    const panel = await enterChronology(app);

    expect(within(panel).getByRole("heading", { name: "Needs a look" })).toBeVisible();
    const lanes = panel.querySelectorAll(".chronology-sequence-lane");
    expect(lanes).toHaveLength(2);
    expect(within(lanes[0] as HTMLElement).getByText("Sony travel card")).toBeVisible();
    expect(within(lanes[0] as HTMLElement).getAllByRole("button", { name: /SONY_/ })).toHaveLength(2);
    expect(within(lanes[1] as HTMLElement).getByText("DJI day card")).toBeVisible();
    expect(within(lanes[1] as HTMLElement).getByRole("button", { name: /DJI_001/ })).toBeVisible();
  });

  it("puts the focused clip and its solved-order neighbours on the compare deck", async () => {
    // The desk's whole subject is the interleave between cards, and SONY_002
    // sits between a Sony clip and a DJI one. Before the deck existed a user
    // judged that from two filenames and a timestamp.
    const app = await renderApp(chronologyTrip().withThumbnail(...sony, ...dji).build());
    await app.user.click(screen.getByText("SONY_002.MP4"));
    const panel = await enterChronology(app);

    const deck = within(panel).getByRole("region", { name: "Compare deck" });
    expect(within(deck).getByText("Clip 2 of 3")).toBeVisible();
    expect(within(deck).getByRole("group", { name: /Focused clip SONY_002\.MP4/ })).toBeVisible();
    expect(within(deck).getByRole("button", { name: /Previous in order: SONY_001\.MP4/ })).toBeVisible();
    expect(within(deck).getByRole("button", { name: /Next in order: DJI_001\.MP4/ })).toBeVisible();
  });

  it("moves the desk's focus when a neighbour on the deck is chosen", async () => {
    const app = await renderApp(chronologyTrip().withThumbnail(...sony, ...dji).build());
    await app.user.click(screen.getByText("SONY_002.MP4"));
    const panel = await enterChronology(app);

    const deck = within(panel).getByRole("region", { name: "Compare deck" });
    await app.user.click(within(deck).getByRole("button", { name: /Next in order: DJI_001\.MP4/ }));

    expect(within(deck).getByRole("group", { name: /Focused clip DJI_001\.MP4/ })).toBeVisible();
    // The inspector follows the deck, so "Why here" explains the clip on screen
    // rather than the one the user last touched on the canvas.
    const inspector = within(panel).getByRole("complementary", { name: "Chronology inspector" });
    expect(within(inspector).getByRole("heading", { name: "DJI_001.MP4" })).toBeVisible();
  });

  it("draws each clip as a frame and keeps its name and time reachable", async () => {
    const app = await renderApp(chronologyTrip().withThumbnail(...sony, ...dji).build());
    const panel = await enterChronology(app);

    const tile = within(panel).getByRole("button", { name: /SONY_001\.MP4, clip 1/ });
    expect(tile.querySelector("img")).toBeInTheDocument();
    expect(tile).toHaveAttribute("title", expect.stringContaining("SONY_001.MP4"));
    expect(tile.querySelector(".chronology-block-ordinal")).toHaveTextContent("001");
  });

  it("asks whether a counter restart was a card change, and shows both answers", async () => {
    // Swapping SD cards through one camera is ordinary practice, so this is a
    // question rather than an error. Before, the row said only what the solver
    // noticed and the action was buried in the Card tab.
    const app = await renderApp(chronologyWithCardBoundary().build());
    const panel = await enterChronology(app);
    const boundary = within(panel).getByRole("group", { name: /card boundary/i });

    expect(within(boundary).getByText(/did you change cards here\?/i)).toBeVisible();
    expect(within(boundary).getByRole("button", { name: /yes.*new card/i })).toBeVisible();
    expect(within(boundary).getByRole("button", { name: /no.*one card/i })).toBeVisible();
  });

  it("does not call an unanswered card boundary a problem, in Review or on the desk", async () => {
    // A counter restart is a question with two ordinary answers. Calling it a
    // problem — the warning chip, the "N problems" heading — told the user
    // something was wrong with footage that is usually completely fine.
    const app = await renderApp(chronologyWithCardBoundary().build());

    expect(screen.queryByRole("button", { name: /chronology problem/i })).toBeNull();
    const chip = await screen.findByRole("button", { name: /1 card boundary.*confirm/i });

    await app.user.click(chip);
    const panel = await screen.findByRole("region", { name: "Chronology calibration" });
    expect(within(panel).getByText(/1 card boundary to confirm/i)).toBeVisible();
    expect(within(panel).queryByText(/order looks trustworthy/i)).toBeNull();
  });

  it("still calls a real contradiction a problem", async () => {
    // The softer framing is for counter restarts only; a solver contradiction
    // is genuinely wrong and keeps the warning chip.
    await renderApp(chronologyWithProblem().build());
    expect(await screen.findByRole("button", { name: /1 chronology problem.*Fix/i })).toBeVisible();
  });

  it("puts the clips either side of the boundary on the deck as the evidence", async () => {
    // The question is answerable by eye — a card swap looks like a scene
    // change — so opening the boundary focuses the first clip of the new run
    // and the deck shows the last clip of the old one beside it.
    const harness = chronologyWithCardBoundary().withThumbnail(...sony, ...dji).build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    await app.user.click(within(panel).getByRole("button", { name: /review this boundary/i }));

    const deck = within(panel).getByRole("region", { name: "Compare deck" });
    expect(within(deck).getByRole("group", { name: /Focused clip DJI_001\.MP4/ })).toBeVisible();
    expect(within(deck).getByRole("button", { name: /Previous in order: SONY_002\.MP4/ })).toBeVisible();
  });

  it("records same-card without a preview, and the question stops being asked", async () => {
    // Nothing moves, so there is nothing to preview — unlike every other write
    // on this surface. The answer still has to be durable.
    const harness = chronologyWithCardBoundary().build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);
    harness.clearLog();

    await app.user.click(within(panel).getByRole("button", { name: /no.*one card/i }));

    expect(harness.commandNames()).toContain("decline_capture_sequence_split");
    expect(harness.calls("decline_capture_sequence_split")[0]).toMatchObject({
      captureSequenceId: "sequence_dji",
      atCounter: 1,
    });
    expect(harness.commandNames()).not.toContain("preview_capture_sequence_split");
    await waitFor(() => {
      expect(within(panel).queryByRole("group", { name: /card boundary/i })).toBeNull();
    });
  });

  it("keeps the same-card answer reversible", async () => {
    const harness = chronologyWithCardBoundary().build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);
    await app.user.click(within(panel).getByRole("button", { name: /no.*one card/i }));
    await waitFor(() => {
      expect(within(panel).queryByRole("group", { name: /card boundary/i })).toBeNull();
    });

    await app.user.click(within(panel).getByRole("button", { name: "Card" }));
    await app.user.click(
      within(panel).getByRole("button", { name: /ask about the restart at 1 again/i }),
    );

    expect(harness.commandNames()).toContain("restore_capture_sequence_split");
    await waitFor(() => {
      expect(within(panel).getByRole("group", { name: /card boundary/i })).toBeVisible();
    });
  });

  it("routes the yes answer through the same preview-then-confirm every write uses", async () => {
    const harness = chronologyWithCardBoundary().build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);
    harness.clearLog();

    await app.user.click(within(panel).getByRole("button", { name: /yes.*new card/i }));

    const dock = await screen.findByRole("dialog", { name: "Chronology impact preview" });
    expect(dock).toBeVisible();
    // Previewed, not written: the desk never mutates before an explicit confirm.
    expect(harness.commandNames()).toContain("preview_capture_sequence_split");
    expect(harness.commandNames()).not.toContain("apply_capture_sequence_split");
  });

  it("shows exact impact, cancels without a write, then applies once and restores the queue", async () => {
    const harness = chronologyTrip().build();
    const app = await renderApp(harness);
    await app.user.click(screen.getByText("SONY_002.MP4"));
    const panel = await enterChronology(app);
    harness.clearLog();

    let impact = await previewClockCorrection(app, panel);
    expect(within(impact).getByText("Nothing has changed yet")).toBeVisible();
    expect(impact.querySelector(".chronology-impact-summary p")).toHaveTextContent("2 clips move");
    expect(within(impact).getByText("#1 → #2")).toBeVisible();
    expect(within(impact).getByText("#2 → #3")).toBeVisible();
    expect(harness.calls("preview_clock_epoch_impact")).toHaveLength(1);
    expect(harness.calls("set_clock_epoch")).toHaveLength(0);

    await app.user.click(within(impact).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog", { name: "Chronology impact preview" })).toBeNull();
    expect(harness.calls("set_clock_epoch")).toHaveLength(0);

    impact = await previewClockCorrection(app, panel);
    await app.user.click(within(impact).getByRole("button", { name: "Apply change" }));

    await waitFor(() => expect(harness.calls("set_clock_epoch")).toHaveLength(1));
    expect(harness.calls("set_clock_epoch")[0]).toEqual({
      projectId: "project_hokkaido",
      preview: { epoch: clockEpoch, factsToken: "facts-clock-sony", impact: clockImpact },
    });
    await waitFor(() => expect(harness.calls("restore_queue_position")).toHaveLength(1));
    expect(harness.calls("capture_queue_position")[0]).toEqual({
      projectId: "project_hokkaido",
      currentAssetId: sony[1],
    });
    expect(harness.state.chronology.overview.assets.map((asset) => asset.assetId)).toEqual([
      dji[0],
      sony[0],
      sony[1],
    ]);

    const safetyOrder = harness.commandNames().filter((command) => [
      "preview_clock_epoch_impact",
      "capture_queue_position",
      "set_clock_epoch",
      "restore_queue_position",
    ].includes(command));
    expect(safetyOrder).toEqual([
      "preview_clock_epoch_impact",
      "preview_clock_epoch_impact",
      "capture_queue_position",
      "set_clock_epoch",
      "restore_queue_position",
    ]);
  });

  it("selects a contiguous card run and previews its visual placement on the canvas", async () => {
    const harness = chronologyTrip()
      .visualPlacementPreview(sony, dji[0], "before", visualPlacementPreview)
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    const secondSony = within(panel).getByRole("button", { name: /SONY_002.MP4, clip 2/ });
    fireEvent.click(secondSony, { shiftKey: true });
    await app.user.click(within(panel).getByRole("button", { name: `Place selected run before ${dji[0]}` }));

    const impact = await screen.findByRole("dialog", { name: "Chronology impact preview" });
    expect(within(impact).getByRole("heading", { name: "Move selected run" })).toBeVisible();
    expect(panel.querySelectorAll(".chronology-ghost-block")).toHaveLength(2);
    expect(panel.querySelectorAll(".chronology-block.will-move")).toHaveLength(2);
    expect(harness.calls("preview_visual_placement")).toEqual([{
      projectId: "project_hokkaido",
      assetIds: sony,
      targetAssetId: dji[0],
      side: "before",
      label: "Placed visually in Chronology",
    }]);

    await app.user.click(within(impact).getByRole("button", { name: "Cancel" }));
    expect(harness.calls("set_clock_epoch")).toHaveLength(0);
  });

  it("offers exact single-clip and clip-and-following card reassignment", async () => {
    const singleOutcomeSequences = [
      { ...sequences[0], assetIds: [sony[1]] },
      { ...sequences[1], assetIds: [dji[0], sony[0]] },
    ];
    const followingOutcomeSequences = [
      { ...sequences[0], assetIds: [] },
      { ...sequences[1], assetIds: [dji[0], ...sony] },
    ];
    const harness = chronologyTrip()
      .captureSequenceReassignment(sony[0], "sequence_dji", singleReassignment, {
        order: [sony[0], sony[1], dji[0]],
        sequences: singleOutcomeSequences,
      })
      .captureSequenceReassignment(sony[0], "sequence_dji", followingReassignment, {
        order: [sony[0], sony[1], dji[0]],
        sequences: followingOutcomeSequences,
      })
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    await app.user.click(within(panel).getByRole("button", { name: "Card" }));
    await app.user.click(within(panel).getByRole("radio", { name: "This clip only" }));
    await app.user.selectOptions(within(panel).getByLabelText("Destination card"), "sequence_dji");
    await app.user.click(within(panel).getByRole("button", { name: "Preview card move" }));
    let impact = await screen.findByRole("dialog", { name: "Chronology impact preview" });
    expect(within(impact).getByRole("heading", { name: "Move this clip to another card" })).toBeVisible();
    expect(harness.calls("preview_capture_sequence_assets")[0]).toEqual({
      projectId: "project_hokkaido",
      fromSequenceId: "sequence_sony",
      intoSequenceId: "sequence_dji",
      assetIds: [sony[0]],
    });
    await app.user.click(within(impact).getByRole("button", { name: "Cancel" }));

    await app.user.click(within(panel).getByRole("radio", { name: "This clip and following" }));
    await app.user.click(within(panel).getByRole("button", { name: "Preview card move" }));
    impact = await screen.findByRole("dialog", { name: "Chronology impact preview" });
    expect(within(impact).getByRole("heading", { name: "Move clip run to another card" })).toBeVisible();
    await app.user.click(within(impact).getByRole("button", { name: "Apply change" }));

    await waitFor(() => expect(harness.calls("reassign_capture_sequence_assets")).toEqual([{
      projectId: "project_hokkaido",
      preview: followingReassignment,
    }]));
    expect(harness.state.chronology.overview.assets
      .filter((asset) => asset.orderKey.captureSequenceId === "sequence_dji")
      .map((asset) => asset.assetId))
      .toEqual([sony[0], sony[1], dji[0]]);
  });

  it("turns a rapid double click into one chronology write", async () => {
    const harness = chronologyTrip().build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);
    const impact = await previewClockCorrection(app, panel);
    harness.next("set_clock_epoch").delayUntilReleased();
    harness.clearLog();

    const apply = within(impact).getByRole("button", { name: "Apply change" });
    fireEvent.click(apply);
    fireEvent.click(apply);

    await waitFor(() => expect(harness.calls("set_clock_epoch")).toHaveLength(1));
    expect(harness.calls("capture_queue_position")).toHaveLength(1);
    expect(apply).toBeDisabled();

    await act(async () => harness.release("set_clock_epoch"));
    await waitFor(() => expect(harness.calls("restore_queue_position")).toHaveLength(1));
    expect(harness.calls("set_clock_epoch")).toHaveLength(1);
  });

  it("blocks a solver-declared contradiction and never captures a queue position", async () => {
    const contradiction = buildContradiction({
      strength: "hard",
      assetIds: [sony[0], dji[0]],
      message: "The candidate order conflicts with the ferry GPS fix",
    });
    const harness = chronologyTrip()
      .impactOnClockPreview("sequence_sony", {
        epoch: clockEpoch,
        factsToken: "facts-clock-sony-blocked",
        impact: {
          changes: clockImpact.changes,
          contradictions: [contradiction],
          blocked: true,
        },
      })
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    const impact = await previewClockCorrection(app, panel);
    expect(within(impact).getByText(/ferry GPS fix/)).toBeVisible();
    expect(within(impact).getByRole("button", { name: "Apply change" })).toBeDisabled();
    expect(harness.calls("set_clock_epoch")).toHaveLength(0);
    expect(harness.calls("capture_queue_position")).toHaveLength(0);
  });
});

describe("chronology-adjacent diagnostics", () => {
  it("acknowledges a media note from its separate disclosure", async () => {
    const harness = chronologyTrip()
      .diagnostic(buildDiagnostic({
        id: "diagnostic_offline_travel",
        severity: "error",
        message: "3 clips on /Volumes/Travel are unreachable",
        volumeHint: "/Volumes/Travel",
        assetIds: [sony[0]],
      }))
      .offlineOriginal(sony[0])
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    await app.user.click(within(panel).getByText("Media and catalog notes · 1"));
    expect(within(panel).getByText(/unreachable/)).toBeVisible();
    await app.user.click(within(panel).getByRole("button", { name: "Acknowledge" }));

    await waitFor(() => expect(harness.state.diagnostics[0].acknowledgedAt).toBe("2026-08-09T12:00:00.000Z"));
    await waitFor(() => expect(within(panel).queryByText("Media and catalog notes · 1")).toBeNull());
  });

  it("relinks offline media without mixing it into the chronology worklist", async () => {
    const harness = chronologyTrip()
      .diagnostic(buildDiagnostic({
        id: "diagnostic_offline_travel",
        severity: "error",
        message: "Clips on /Volumes/Travel are unreachable",
        volumeHint: "/Volumes/Travel",
        assetIds: [sony[0], sony[1]],
      }))
      .offlineOriginal(sony[0], sony[1])
      .relinkableAt("/Volumes/Travel2", sony[0], sony[1])
      .dialogAnswers("/Volumes/Travel2")
      .build();
    const app = await renderApp(harness);
    const panel = await enterChronology(app);

    expect(within(panel).getByRole("heading", { name: "All in order" })).toBeVisible();
    await app.user.click(within(panel).getByText("Media and catalog notes · 1"));
    await app.user.click(within(panel).getByRole("button", { name: "Locate originals…" }));

    await waitFor(() => expect(harness.state.assets[sony[0]].originalAvailable).toBe(true));
    expect(harness.state.assets[sony[1]].originalAvailable).toBe(true);
    expect(await within(panel).findByText("Reconnected 2 clips")).toBeVisible();
  });
});
