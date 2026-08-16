/**
 * CUJ: reusing the project's label vocabulary instead of fragmenting it.
 *
 * The failure this prevents is slow and invisible: over a few thousand clips a
 * free-text field accumulates `night walk`, `nightwalk` and `Night Walk`, and
 * the filter the user built the vocabulary *for* silently stops finding two
 * thirds of the footage. Nothing here renames anything — a label is user data
 * — so what is asserted is that the existing spelling is offered, is reachable
 * from the keys, and wins over a new casing of itself.
 *
 * The fast loop is the constraint. `L` → type → Enter must still add exactly
 * what was typed; a suggestion only wins when the user deliberately moved to
 * it.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, renderApp } from "../harness";

const clips = [
  "asset_sony_a7c_ii_001",
  "asset_sony_a7c_ii_002",
  "asset_sony_a7c_ii_003",
  "asset_sony_a7c_ii_004",
];

/** A project part-way through a labelling pass, with a vocabulary in progress. */
function partWayThrough() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 4, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" })
    .withLabels(clips[1], "night walk", "market")
    .withLabels(clips[2], "night walk")
    .withLabels(clips[3], "night train")
    .build();
}

const field = () => screen.getByLabelText("Add label");
const list = () => screen.queryByRole("listbox", { name: /Labels already in this project/ });

describe("offering the project's vocabulary", () => {
  it("stays out of the way until something is typed", async () => {
    const app = await renderApp(partWayThrough());

    await app.user.keyboard("l");

    // `L` → type → Enter is the loop; a list waiting in front of the first
    // keystroke would put a highlighted suggestion where the typing goes.
    expect(field()).toHaveFocus();
    expect(list()).toBeNull();
  });

  it("shows the matching labels the project already uses, most-used first", async () => {
    const app = await renderApp(partWayThrough());

    await app.user.type(field(), "night");

    const options = within(list()!).getAllByRole("option");
    expect(options.map((option) => option.textContent)).toEqual(["night walk2", "night train1"]);
  });

  it("offers nothing for a label the project has never used", async () => {
    const app = await renderApp(partWayThrough());

    await app.user.type(field(), "harbour");

    expect(list()).toBeNull();
  });

  it("does not offer a label this clip already carries", async () => {
    const app = await renderApp(partWayThrough());
    await app.user.click(screen.getByText("SONY_002.MP4"));
    await waitFor(() => expect(screen.getByLabelText("Clip labels").textContent).toContain("market"));

    await app.user.type(field(), "mark");

    expect(list()).toBeNull();
  });
});

describe("choosing from the keyboard", () => {
  it("adds exactly what was typed when no suggestion was chosen", async () => {
    const harness = partWayThrough();
    const app = await renderApp(harness);

    await app.user.type(field(), "night{Enter}");

    // The list was open and "night walk" was on top; Enter still committed the
    // word the user actually typed.
    await waitFor(() => expect(harness.state.assets[clips[0]].labels).toEqual(["night"]));
  });

  it("adds the highlighted suggestion once the user arrows to it", async () => {
    const harness = partWayThrough();
    const app = await renderApp(harness);

    await app.user.type(field(), "night");
    await app.user.keyboard("{ArrowDown}{Enter}");

    await waitFor(() => expect(harness.state.assets[clips[0]].labels).toEqual(["night walk"]));
    expect(field()).toHaveValue("");
  });

  it("steps down the list and back off the end of it", async () => {
    const harness = partWayThrough();
    const app = await renderApp(harness);

    await app.user.type(field(), "night");
    await app.user.keyboard("{ArrowDown}{ArrowDown}{ArrowDown}{Enter}");

    // Three steps through a two-item list lands back on "nothing highlighted",
    // so the way back to committing the typed word is reachable from the keys.
    await waitFor(() => expect(harness.state.assets[clips[0]].labels).toEqual(["night"]));
  });

  it("closes the list on Escape without leaving the review layout", async () => {
    const app = await renderApp(partWayThrough());
    await app.user.type(field(), "night");
    expect(list()).not.toBeNull();

    await app.user.keyboard("{Escape}");

    expect(list()).toBeNull();
    // The shell owns Escape now; this one belonged to the field, and must not
    // have travelled on to the rest of the interface.
    expect(field()).toHaveValue("night");
    expect(screen.getByRole("listbox", { name: "Clips" })).toBeVisible();
  });
});

describe("choosing with the mouse", () => {
  it("adds the label that was clicked", async () => {
    const harness = partWayThrough();
    const app = await renderApp(harness);
    await app.user.type(field(), "night");

    await app.user.click(within(list()!).getByText("night train"));

    await waitFor(() => expect(harness.state.assets[clips[0]].labels).toEqual(["night train"]));
    expect(list()).toBeNull();
  });
});

describe("preferring the spelling the project already has", () => {
  it("joins the existing label when only the casing differs", async () => {
    const harness = partWayThrough();
    const app = await renderApp(harness);

    await app.user.type(field(), "Market{Enter}");

    // Not "Market" beside "market": that is the duplicate this feature exists
    // to stop, and it would be invisible until a filter came up short.
    await waitFor(() => expect(harness.state.assets[clips[0]].labels).toEqual(["market"]));
  });

  it("still writes a genuinely new label exactly as typed", async () => {
    const harness = partWayThrough();
    const app = await renderApp(harness);

    await app.user.type(field(), "Harbour At Dawn{Enter}");

    await waitFor(() => expect(harness.state.assets[clips[0]].labels).toEqual(["Harbour At Dawn"]));
  });
});
