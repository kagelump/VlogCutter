/**
 * The chronology write path, at the adapter boundary.
 *
 * `chronologyApi.ts` lines 66–147 — every mutation except the clock-epoch pair
 * — had no coverage at all: sequence splitting and merging, clip time
 * exceptions, trip phases, and event-order constraints. Reads were exercised by
 * the chronology CUJ, writes were not.
 *
 * These are thin `invoke` wrappers, so the thing worth proving is the thing
 * that can silently break: the command name and the exact argument object each
 * function sends. `contract.test.ts` proves every name the frontend uses is
 * registered in Rust and handled here; nothing proved that
 * `splitCaptureSequence` sends `sourceSequenceId` rather than `sequenceId`. A
 * renamed field would compile, pass every existing test, and fail only in the
 * real app — the harness rejects an argument sent under the wrong name, which
 * is what makes calling through it evidence rather than exercise.
 *
 * Deliberately no UI. These assertions are about the adapter's contract with
 * Rust, so they hold across any redesign of the calibration surface, and this
 * file does not compete for the chronology UX's files.
 */

import { afterEach, describe, expect, it } from "vitest";
import { chronologyApi } from "../../chronologyApi";
import type { TripPhase } from "../../chronologyTypes";
import { desktopScenario, type DesktopHarness } from "../harness";

const sony = ["asset_sony_a7c_ii_001", "asset_sony_a7c_ii_002", "asset_sony_a7c_ii_003"];
const PROJECT = "project_hokkaido";

let installed: DesktopHarness | null = null;

afterEach(() => {
  installed?.uninstall();
  installed = null;
});

/** A three-clip trip with two sequences and one phase covering the whole thing. */
function trip(phases: TripPhase[] = []) {
  const harness = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: PROJECT })
    .camera("Sony A7C II", { clips: 3 })
    .chronology({
      order: sony,
      sequences: [
        { id: "sequence_sony", cameraKey: "Sony A7C II", label: "Sony A7C II", volumeHint: "/Volumes/Travel" },
        { id: "sequence_dji", cameraKey: "DJI Pocket 3", label: "DJI Pocket 3" },
      ],
      phases,
      proposedSplits: [
        {
          captureSequenceId: "sequence_sony",
          atCounter: 2,
          reason: "card swap",
          affectedAssetIds: [sony[1], sony[2]],
        },
      ],
    })
    .build();
  installed = harness.install();
  harness.clearLog();
  return harness;
}

function wholeTrip(): TripPhase {
  return {
    id: "phase_trip",
    label: "Hokkaido",
    kind: "location",
    startAnchorAssetId: sony[0],
    endAnchorAssetId: sony[2],
    startTripTime: "2026-02-08T00:10:00.000Z",
    endTripTime: "2026-02-08T00:20:00.000Z",
    timezone: "Asia/Tokyo",
    originTimezone: null,
    destinationTimezone: null,
    displayMode: "local",
  };
}

/** The single argument object a command was called with, asserted to be the only one. */
function onlyCall(harness: DesktopHarness, command: string) {
  const calls = harness.calls(command);
  expect(calls, `expected exactly one ${command}`).toHaveLength(1);
  return calls[0];
}

describe("capture sequence edits", () => {
  it("splits a sequence at a counter, sending the source, counter and label", async () => {
    const harness = trip();

    const created = await chronologyApi.splitCaptureSequence(PROJECT, "sequence_sony", 2, "Sony, card 2");

    expect(onlyCall(harness, "split_capture_sequence")).toEqual({
      projectId: PROJECT,
      sourceSequenceId: "sequence_sony",
      atCounter: 2,
      label: "Sony, card 2",
    });
    expect(created).toMatchObject({ id: "sequence_sony_split_2", label: "Sony, card 2" });
    // The backend's own DTO, not a reshaped one: the adapter adds nothing.
    expect(harness.state.chronology.overview.sequences).toContainEqual(created);
    // The proposal this edit answers is gone, which is how the panel knows.
    expect(harness.state.chronology.overview.proposedSplits).toEqual([]);
  });

  it("merges one sequence into another, keeping the two ids distinct", async () => {
    const harness = trip();

    await chronologyApi.mergeCaptureSequences(PROJECT, "sequence_dji", "sequence_sony");

    expect(onlyCall(harness, "merge_capture_sequences")).toEqual({
      projectId: PROJECT,
      fromSequenceId: "sequence_dji",
      intoSequenceId: "sequence_sony",
    });
    // Direction matters: the *from* sequence is the one that stops existing.
    const remaining = harness.state.chronology.overview.sequences.map((sequence) => sequence.id);
    expect(remaining).toEqual(["sequence_sony"]);
  });

  it("surfaces a rejected split rather than resolving to nothing", async () => {
    const harness = trip();

    await expect(
      chronologyApi.splitCaptureSequence(PROJECT, "sequence_that_left", 2, "orphan"),
    ).rejects.toThrow(/Unknown capture sequence: sequence_that_left/);
    expect(harness.state.chronology.overview.sequences).toHaveLength(2);
  });
});

describe("clip time exceptions", () => {
  it("pins one clip's time with the reason attached", async () => {
    const harness = trip();

    const exception = await chronologyApi.setClipTimeException(
      PROJECT,
      sony[1],
      "2026-02-08T09:15:00.000Z",
      "Timestamped from the ticket stub",
    );

    expect(onlyCall(harness, "set_clip_time_exception")).toEqual({
      projectId: PROJECT,
      assetId: sony[1],
      tripTime: "2026-02-08T09:15:00.000Z",
      reason: "Timestamped from the ticket stub",
    });
    // The reason travels with the exception; an override with no recorded why
    // is the thing this field exists to prevent.
    expect(exception).toEqual({
      assetId: sony[1],
      tripTime: "2026-02-08T09:15:00.000Z",
      reason: "Timestamped from the ticket stub",
    });
    expect(harness.state.chronology.overview.exceptions).toEqual([exception]);
  });

  it("replaces rather than duplicates when the same clip is pinned twice", async () => {
    const harness = trip();

    await chronologyApi.setClipTimeException(PROJECT, sony[1], "2026-02-08T09:15:00.000Z", "first");
    await chronologyApi.setClipTimeException(PROJECT, sony[1], "2026-02-08T10:45:00.000Z", "corrected");

    expect(harness.state.chronology.overview.exceptions).toEqual([
      { assetId: sony[1], tripTime: "2026-02-08T10:45:00.000Z", reason: "corrected" },
    ]);
  });

  it("clears one clip's exception by asset, leaving the others", async () => {
    const harness = trip();
    await chronologyApi.setClipTimeException(PROJECT, sony[0], "2026-02-08T08:00:00.000Z", "kept");
    await chronologyApi.setClipTimeException(PROJECT, sony[1], "2026-02-08T09:15:00.000Z", "dropped");

    await chronologyApi.clearClipTimeException(PROJECT, sony[1]);

    expect(onlyCall(harness, "clear_clip_time_exception")).toEqual({
      projectId: PROJECT,
      assetId: sony[1],
    });
    expect(harness.state.chronology.overview.exceptions.map((found) => found.assetId)).toEqual([sony[0]]);
  });
});

describe("trip phases", () => {
  it("writes a phase as one object argument, not as flattened fields", async () => {
    const harness = trip();
    const phase = { ...wholeTrip(), id: "" };

    const stored = await chronologyApi.setTripPhase(PROJECT, phase);

    // `phase` is nested: flattening it would be accepted by a permissive
    // backend and silently drop the timezone fields.
    expect(onlyCall(harness, "set_trip_phase")).toEqual({ projectId: PROJECT, phase });
    expect(stored).toMatchObject({
      id: "phase_1",
      label: "Hokkaido",
      kind: "location",
      timezone: "Asia/Tokyo",
      displayMode: "local",
    });
    expect(harness.state.chronology.overview.phases).toEqual([stored]);
  });

  it("splits a phase through a boundary clip, sending the direction verbatim", async () => {
    const harness = trip([wholeTrip()]);
    const newPhase = { ...wholeTrip(), id: "", label: "Sapporo", kind: "transit" as const };

    const authored = await chronologyApi.splitTripPhase(PROJECT, "phase_trip", sony[1], "through", newPhase);

    expect(onlyCall(harness, "split_trip_phase")).toEqual({
      projectId: PROJECT,
      existingPhaseId: "phase_trip",
      boundaryAssetId: sony[1],
      direction: "through",
      newPhase,
    });
    // "through" means the new phase ends at the boundary and the old one
    // resumes after it — the direction is not cosmetic.
    expect(authored).toMatchObject({
      label: "Sapporo",
      startAnchorAssetId: sony[0],
      endAnchorAssetId: sony[1],
    });
    expect(harness.state.chronology.overview.phases).toContainEqual(
      expect.objectContaining({ id: "phase_trip", startAnchorAssetId: sony[2], endAnchorAssetId: sony[2] }),
    );
  });

  it("splits from a boundary clip, which anchors the new phase the other way", async () => {
    const harness = trip([wholeTrip()]);
    const newPhase = { ...wholeTrip(), id: "", label: "Otaru", kind: "location" as const };

    const authored = await chronologyApi.splitTripPhase(PROJECT, "phase_trip", sony[1], "from", newPhase);

    expect(harness.calls("split_trip_phase")[0]).toMatchObject({ direction: "from" });
    expect(authored).toMatchObject({ startAnchorAssetId: sony[1], endAnchorAssetId: sony[2] });
    expect(harness.state.chronology.overview.phases).toContainEqual(
      expect.objectContaining({ id: "phase_trip", startAnchorAssetId: sony[0], endAnchorAssetId: sony[0] }),
    );
  });

  it("refuses a boundary outside the phase rather than authoring a nonsense span", async () => {
    const harness = trip([{ ...wholeTrip(), startAnchorAssetId: sony[0], endAnchorAssetId: sony[0] }]);

    await expect(
      chronologyApi.splitTripPhase(PROJECT, "phase_trip", sony[2], "through", { ...wholeTrip(), id: "" }),
    ).rejects.toThrow(/is not inside trip phase phase_trip/);
    expect(harness.state.chronology.overview.phases).toHaveLength(1);
  });

  it("removes a phase by id", async () => {
    const harness = trip([wholeTrip()]);

    await chronologyApi.removeTripPhase(PROJECT, "phase_trip");

    expect(onlyCall(harness, "remove_trip_phase")).toEqual({ projectId: PROJECT, phaseId: "phase_trip" });
    expect(harness.state.chronology.overview.phases).toEqual([]);
  });
});

describe("event order constraints", () => {
  it("sends earlier and later as distinct fields, with strength and source", async () => {
    const harness = trip();

    const constraint = await chronologyApi.setEventOrderConstraint(
      PROJECT,
      sony[2],
      sony[0],
      "hard",
      "user_set",
    );

    // Swapping these two is a silent semantic inversion — the constraint would
    // still write, and order the trip backwards.
    expect(onlyCall(harness, "set_event_order_constraint")).toEqual({
      projectId: PROJECT,
      earlierAssetId: sony[2],
      laterAssetId: sony[0],
      strength: "hard",
      source: "user_set",
    });
    expect(constraint).toMatchObject({
      earlierAssetId: sony[2],
      laterAssetId: sony[0],
      strength: "hard",
      source: "user_set",
    });
    expect(harness.state.chronology.overview.constraints).toEqual([constraint]);
  });

  it("carries a soft constraint's strength through unchanged", async () => {
    const harness = trip();

    await chronologyApi.setEventOrderConstraint(PROJECT, sony[0], sony[1], "soft", "inferred_from_notes");

    expect(harness.calls("set_event_order_constraint")[0]).toMatchObject({
      strength: "soft",
      source: "inferred_from_notes",
    });
  });

  it("clears a constraint by its own id, not by the clips it relates", async () => {
    const harness = trip();
    const first = await chronologyApi.setEventOrderConstraint(PROJECT, sony[0], sony[1], "hard", "user_set");
    await chronologyApi.setEventOrderConstraint(PROJECT, sony[1], sony[2], "hard", "user_set");

    await chronologyApi.clearEventOrderConstraint(PROJECT, first!.id);

    expect(onlyCall(harness, "clear_event_order_constraint")).toEqual({
      projectId: PROJECT,
      constraintId: first!.id,
    });
    expect(harness.state.chronology.overview.constraints.map((found) => found.id)).toEqual([
      "constraint_2",
    ]);
  });
});

describe("edit plan revision history", () => {
  it("lists every revision, not only the active one", async () => {
    const harness = trip();
    const first = await chronologyApi.createEditPlanRevision(
      PROJECT,
      [{ assetId: sony[0], sourceInSeconds: 0, sourceOutSeconds: 4, rationale: "opener" }],
      null,
    );
    await chronologyApi.createEditPlanRevision(
      PROJECT,
      [{ assetId: sony[1], sourceInSeconds: 1, sourceOutSeconds: 5, rationale: "second pass" }],
      null,
    );
    // Creating a revision does not activate it — that is a separate, explicit
    // step — so the history and the active plan are genuinely different reads.
    await chronologyApi.activateEditPlanRevision(PROJECT, first!.revisionId, null);

    const revisions = await chronologyApi.editPlanRevisions(PROJECT);

    expect(onlyCall(harness, "edit_plan_revisions")).toEqual({ projectId: PROJECT });
    expect(revisions.map((revision) => revision.revisionId)).toEqual(["revision_1", "revision_2"]);
    expect(revisions.filter((revision) => revision.active)).toHaveLength(1);
    expect((await chronologyApi.activeEditPlan(PROJECT))!.revisionId).toBe(first!.revisionId);
  });
});

describe("the desktop predicate", () => {
  it("reports the runtime it is actually in, which the harness makes true", () => {
    // `isDesktop` gates behaviour elsewhere; with the harness installed the
    // Tauri internals are present, so it must agree.
    trip();
    expect(chronologyApi.isDesktop()).toBe(true);
  });

  it("is false with no Tauri runtime installed", () => {
    expect("__TAURI_INTERNALS__" in window).toBe(false);
    expect(chronologyApi.isDesktop()).toBe(false);
  });
});
