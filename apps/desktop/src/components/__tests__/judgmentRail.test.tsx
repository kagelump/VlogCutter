import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "../../App";
import { api } from "../../api";

/**
 * The right rail's contents, as opposed to its layout.
 *
 * Both cases here are the same reported bug: the AI suggestion chips appeared
 * for a moment and then vanished. One half was a stale read rendered under the
 * wrong clip; the other was a refetch storm keyed on the whole asset list,
 * where a single failure blanked a panel that had already loaded.
 */

async function renderApp() {
  const rendered = render(<App />);
  await screen.findByText("Train pulls into Otaru station");
  return rendered;
}

function suggestions(container: HTMLElement) {
  const strip = container.querySelector<HTMLElement>(".ai-strip");
  return strip ? [...within(strip).queryAllByRole("button")].map((button) => button.textContent) : [];
}

afterEach(() => vi.restoreAllMocks());

describe("clip detail follows the selection", () => {
  it("never shows one clip's suggestions under another clip's title", async () => {
    const { container } = await renderApp();
    await waitFor(() => expect(suggestions(container)).toContain("station"));

    // Synchronous on purpose: the read for the newly selected clip has not
    // resolved yet, which is exactly the moment the stale chips used to show.
    fireEvent.click(screen.getByText("DJI_0342.MP4"));
    expect(suggestions(container)).toEqual([]);

    await waitFor(() => expect(screen.getByLabelText("Clip title")).toHaveValue(""));
    expect(suggestions(container)).toEqual([]);
  });

  it("brings them back on returning to the clip that has them", async () => {
    const { container } = await renderApp();
    fireEvent.click(screen.getByText("DJI_0342.MP4"));
    fireEvent.click(screen.getByText("Train pulls into Otaru station"));
    await waitFor(() => expect(suggestions(container)).toContain("station"));
  });
});

describe("clip detail is not refetched by unrelated asset writes", () => {
  it("does not read the observation again on every keystroke in the notes field", async () => {
    await renderApp();
    const analysis = vi.spyOn(api, "assetAnalysis");
    const notes = screen.getByLabelText("Notes");

    for (const value of ["a", "ab", "abc", "abcd", "abcde"]) {
      fireEvent.change(notes, { target: { value } });
    }
    await waitFor(() => expect(notes).toHaveValue("abcde"));

    // Each keystroke rewrites the asset list. Keyed on that list, this was two
    // IPC calls and two SQLite opens per character typed.
    expect(analysis).not.toHaveBeenCalled();
  });

  it("still reads it again when the selection changes", async () => {
    await renderApp();
    const analysis = vi.spyOn(api, "assetAnalysis");
    fireEvent.click(screen.getByText("DJI_0342.MP4"));
    await waitFor(() => expect(analysis).toHaveBeenCalledWith("asset_demo_1"));
  });
});
