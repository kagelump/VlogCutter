/**
 * "Show this in the timezone I was actually in." (#72)
 *
 * The Story Ribbon's selected-position readout used to clip a normal full
 * timestamp at a fixed width, and the part it ate was the timezone — leaving a
 * wall clock nobody could interpret and no way to improve it. This journey is
 * the answer: read the zone, click it, say where you were, watch the readout
 * change — and confirm that nothing about the clips themselves moved.
 *
 * Display timezone and camera clock correction are different facts. The
 * assertion that matters most below is the one about what did *not* happen.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, queueFilenames, renderApp } from "../harness";

const SONY = ["asset_sony_a7c_ii_001", "asset_sony_a7c_ii_002", "asset_sony_a7c_ii_003"];
const SEQUENCES = [{ id: "sequence_sony", cameraKey: "Sony A7C II", label: "Sony card", assetIds: SONY }];

/** A phase covering the first two clips only, so the third is uncovered. */
const BARCELONA = {
  id: "phase_barcelona",
  label: "Barcelona",
  kind: "location" as const,
  startAnchorAssetId: SONY[0],
  endAnchorAssetId: SONY[1],
  startTripTime: "2026-02-08T00:10:00.000Z",
  endTripTime: "2026-02-08T00:20:00.000Z",
  timezone: "Europe/Madrid",
  originTimezone: null,
  destinationTimezone: null,
  displayMode: "local" as const,
};

function trip(phases = [BARCELONA]) {
  return desktopScenario({ now: "2026-08-20T12:00:00.000Z" })
    .project("Spain 2026", { id: "project_spain" })
    // Capture times and solved trip times deliberately agree: this project has
    // no clock correction, so a phase's bounds (built from the clips' own
    // times) line up with where the solver placed them.
    .camera("Sony A7C II", { clips: 3, withProxy: true, startAt: "2026-02-08T00:10:00.000Z", spacingSeconds: 600 })
    .chronology({ order: SONY, sequences: SEQUENCES, spacingSeconds: 600, phases })
    .build();
}

function readout() {
  return document.querySelector(".chronology-position")!;
}

function zoneButton() {
  return readout().querySelector<HTMLButtonElement>("button.position-zone")!;
}

async function select(app: Awaited<ReturnType<typeof renderApp>>, filename: string) {
  const row = screen.getByRole("option", { name: new RegExp(filename) });
  await app.user.click(row);
  await waitFor(() => expect(readout().textContent).toContain("#"));
}

describe("reading and setting the display timezone", () => {
  it("shows the whole reading: ordinal, wall clock, and an unabbreviated zone", async () => {
    const app = await renderApp(trip());
    await select(app, "SONY_001.MP4");

    expect(readout().querySelector(".position-ordinal")!.textContent).toBe("#1");
    expect(readout().querySelector(".position-clock")!.textContent).toMatch(/^\d{2}:\d{2}:\d{2}$/);
    // The defect was `· U…`. The zone is whole, and it names a real zone.
    expect(zoneButton().textContent).toContain("Europe/Madrid");
    expect(zoneButton().getAttribute("data-authoritative")).toBe("true");
  });

  it("marks a clip no trip context covers as an open question, not a fact", async () => {
    const app = await renderApp(trip());
    await select(app, "SONY_003.MP4");

    expect(zoneButton().textContent).toContain("UTC");
    expect(zoneButton().textContent).toContain("set zone");
    expect(zoneButton().getAttribute("data-authoritative")).toBe("false");
  });

  it("routes the zone through a trip context whose range the user confirms", async () => {
    const app = await renderApp(trip());
    await select(app, "SONY_003.MP4");
    await app.user.click(zoneButton());

    // Scoped to a named range, not to this one clip and not to camera clocks.
    const dialog = await screen.findByRole("dialog", { name: /Add context to the ribbon/ });
    expect(within(dialog).getByText(/Suggested from “Barcelona”/)).toBeVisible();

    // A suggestion is offered, never filled in: the user has to take it.
    const timezone = within(dialog).getByLabelText(/^Timezone/) as HTMLInputElement;
    expect(timezone.value).toBe("");
    await app.user.click(within(dialog).getByRole("button", { name: "Use Europe/Madrid" }));
    expect(timezone.value).toBe("Europe/Madrid");
  });

  it("changes what the clip is shown in without moving anything", async () => {
    const harness = trip();
    const app = await renderApp(harness);
    const orderBefore = queueFilenames();
    const timesBefore = harness.state.chronology.overview.assets.map((asset) => asset.tripTime);
    const epochsBefore = harness.state.chronology.overview.epochs.length;

    await select(app, "SONY_003.MP4");
    await app.user.click(zoneButton());
    const dialog = await screen.findByRole("dialog", { name: /Add context to the ribbon/ });
    await app.user.type(within(dialog).getByLabelText(/^Name/), "Girona");
    await app.user.click(within(dialog).getByRole("button", { name: "Use Europe/Madrid" }));
    await app.user.click(within(dialog).getByRole("button", { name: "Add phase" }));

    await waitFor(() => expect(zoneButton().textContent).toContain("Europe/Madrid"));
    expect(zoneButton().getAttribute("data-authoritative")).toBe("true");

    // The whole point: a display choice is not a clock correction.
    expect(queueFilenames()).toEqual(orderBefore);
    expect(harness.state.chronology.overview.assets.map((asset) => asset.tripTime)).toEqual(timesBefore);
    expect(harness.state.chronology.overview.epochs).toHaveLength(epochsBefore);
  });

  it("proposes nothing when the project has no evidence to propose from", async () => {
    const app = await renderApp(trip([]));
    await select(app, "SONY_001.MP4");
    expect(zoneButton().textContent).toContain("UTC");

    await app.user.click(zoneButton());
    const dialog = await screen.findByRole("dialog", { name: /Add context to the ribbon/ });
    // Never the host's timezone, and never a guess with nothing behind it.
    expect(within(dialog).queryByText(/Suggested from/)).toBeNull();
    expect((within(dialog).getByLabelText(/^Timezone/) as HTMLInputElement).value).toBe("");
  });

  it("opens the covering context for a clip that already has one, rather than a new one", async () => {
    const app = await renderApp(trip());
    await select(app, "SONY_001.MP4");
    await app.user.click(zoneButton());

    const dialog = await screen.findByRole("dialog", { name: /Edit Barcelona/ });
    expect((within(dialog).getByLabelText(/^Timezone/) as HTMLInputElement).value).toBe("Europe/Madrid");
    // An answer the user already gave is not second-guessed with a suggestion.
    expect(within(dialog).queryByText(/Suggested from/)).toBeNull();
  });

  it("lets the user go back to UTC explicitly", async () => {
    const app = await renderApp(trip());
    await select(app, "SONY_001.MP4");
    await app.user.click(zoneButton());
    const dialog = await screen.findByRole("dialog", { name: /Edit Barcelona/ });

    const timezone = within(dialog).getByLabelText(/^Timezone/);
    await app.user.clear(timezone);
    await app.user.click(within(dialog).getByRole("button", { name: "Save phase" }));

    await waitFor(() => expect(zoneButton().textContent).toContain("UTC"));
  });
});
