/**
 * CUJ: searching for something half-remembered.
 *
 * Filtering the queue was already there; what was missing is the half that
 * makes a result useful. A row that says only "this one matched" sends the
 * reader into the clip to find out whether the phrase was something they wrote,
 * something that was said, or the camera's name — and a spoken match left them
 * scanning a transcript that had already found the answer.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, renderApp } from "../harness";

const sony = ["asset_sony_a7c_ii_001", "asset_sony_a7c_ii_002", "asset_sony_a7c_ii_003"];

/** Three clips that match the same phrase for three different reasons. */
function project() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 3, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" })
    .withNote(sony[0], "The canal path is worth a second pass at dusk")
    .withTranscript(sony[1], {
      segments: [
        { startMs: 2_000, text: "the canal is over that way" },
        { startMs: 30_000, text: "nothing here" },
        { startMs: 61_500, text: "back at the canal now" },
      ],
    })
    .withLabels(sony[2], "canal")
    .build();
}

const search = () => screen.getByRole("textbox", { name: /Filter clips/i });
// Scoped to the queue: the project picker's `<option>`s carry the same role.
const rows = () => within(screen.getByRole("listbox", { name: "Clips" })).getAllByRole("option");
const rowFor = (filename: string) => rows().find((row) => row.textContent?.includes(filename));

describe("explaining why a clip matched", () => {
  it("names the field and shows the surrounding words", async () => {
    const app = await renderApp(project());

    await app.user.type(search(), "canal");

    await waitFor(() => expect(rows()).toHaveLength(3));
    const note = rowFor("SONY_001.MP4")!.querySelector(".match-snippet")!;
    expect(note.querySelector("em")).toHaveTextContent("note");
    expect(note.textContent).toContain("The canal path is worth a second pass");
    expect(rowFor("SONY_002.MP4")!.querySelector(".match-snippet em")).toHaveTextContent("transcript");
    expect(rowFor("SONY_003.MP4")!.querySelector(".match-snippet em")).toHaveTextContent("label");
  });

  it("marks the matched words inside the snippet", async () => {
    const app = await renderApp(project());

    await app.user.type(search(), "canal");

    await waitFor(() => expect(rowFor("SONY_001.MP4")!.querySelector("mark")).not.toBeNull());
    expect(rowFor("SONY_001.MP4")!.querySelector("mark")).toHaveTextContent("canal");
  });

  it("leaves an unfiltered queue exactly as it was", async () => {
    const app = await renderApp(project());
    await app.user.type(search(), "canal");
    await waitFor(() => expect(rows()).toHaveLength(3));

    await app.user.clear(search());

    // The row's four facts are what make the queue readable by position at
    // speed; a snippet is only worth its space while a question is being asked.
    await waitFor(() => expect(rows()).toHaveLength(3));
    expect(rows().some((row) => row.querySelector(".match-snippet"))).toBe(false);
  });
});

describe("jumping to where the phrase was said", () => {
  it("moves the playhead to the first spoken match and opens the transcript", async () => {
    const app = await renderApp(project());
    await app.user.type(search(), "canal");
    await waitFor(() => expect(rows()).toHaveLength(3));
    await app.user.click(screen.getByText("SONY_002.MP4"));

    await app.user.click(search());
    await app.user.keyboard("{Enter}");

    expect(await screen.findByText("Spoken match 1 of 2")).toBeVisible();
    const panel = screen.getByRole("tabpanel");
    expect(within(panel).getByText("the canal is over that way")).toBeVisible();
  });

  it("cycles through the matches and back to the first", async () => {
    const app = await renderApp(project());
    await app.user.type(search(), "canal");
    await waitFor(() => expect(rows()).toHaveLength(3));
    await app.user.click(screen.getByText("SONY_002.MP4"));
    await app.user.click(search());

    await app.user.keyboard("{Enter}");
    await app.user.keyboard("{Enter}");
    expect(await screen.findByText("Spoken match 2 of 2")).toBeVisible();

    // "Say that again" is why anyone presses this twice; a control that went
    // dead on the last match would make the reader work out where they are.
    await app.user.keyboard("{Enter}");
    expect(await screen.findByText("Spoken match 1 of 2")).toBeVisible();
  });

  it("marks the matching lines in the transcript itself", async () => {
    const app = await renderApp(project());
    await app.user.type(search(), "canal");
    await waitFor(() => expect(rows()).toHaveLength(3));
    await app.user.click(screen.getByText("SONY_002.MP4"));
    await app.user.click(search());
    await app.user.keyboard("{Enter}");

    const panel = await screen.findByRole("tabpanel");
    const marked = [...panel.querySelectorAll(".segment-list li.matched")];
    expect(marked.map((row) => row.textContent)).toEqual([
      expect.stringContaining("the canal is over that way"),
      expect.stringContaining("back at the canal now"),
    ]);
  });

  it("says so plainly when the clip matched somewhere other than its speech", async () => {
    const app = await renderApp(project());
    await app.user.type(search(), "canal");
    await waitFor(() => expect(rows()).toHaveLength(3));
    await app.user.click(screen.getByText("SONY_001.MP4"));
    await app.user.click(search());

    await app.user.keyboard("{Enter}");

    // The clip is in the results, so it matched something; naming that is more
    // use than a keystroke that appears to do nothing.
    expect(await screen.findByText(/matched elsewhere/)).toBeVisible();
  });

  it("starts the cycle again for a different phrase", async () => {
    const app = await renderApp(project());
    await app.user.type(search(), "canal");
    await waitFor(() => expect(rows()).toHaveLength(3));
    await app.user.click(screen.getByText("SONY_002.MP4"));
    await app.user.click(search());
    await app.user.keyboard("{Enter}");
    await app.user.keyboard("{Enter}");
    expect(await screen.findByText("Spoken match 2 of 2")).toBeVisible();

    await app.user.clear(search());
    await app.user.type(search(), "canal now");
    await app.user.keyboard("{Enter}");

    expect(await screen.findByText("Spoken match 1 of 1")).toBeVisible();
  });
});
