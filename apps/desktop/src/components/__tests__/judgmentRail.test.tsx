import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../../api";
import { desktopScenario, renderApp } from "../../test/harness";

/**
 * The right rail's contents, as opposed to its layout.
 *
 * Both cases here are the same reported bug: the AI suggestion chips appeared
 * for a moment and then vanished. One half was a stale read rendered under the
 * wrong clip; the other was a refetch storm keyed on the whole asset list,
 * where a single failure blanked a panel that had already loaded.
 *
 * The clips come from the CUJ harness, so the fixture these assertions rest on
 * is the one the whole suite uses rather than a set of demo values that only
 * this file knew about.
 */

const OBSERVED = "asset_sony_a7c_ii_001";
const UNOBSERVED = "asset_dji_pocket_3_001";

async function bootApp() {
  const harness = desktopScenario()
    .project("Hokkaido field test")
    .camera("Sony A7C II", { clips: 2, startAt: "2026-02-08T00:14:08.000Z", withProxy: true })
    .camera("DJI Pocket 3", { clips: 1, startAt: "2026-02-08T00:18:41.000Z", withProxy: true })
    .withSummary(OBSERVED, "Train pulls into Otaru station")
    .withAnalysis(OBSERVED, { aiLabels: ["station", "arrival"] })
    .selectedClip(OBSERVED)
    .build();
  const booted = await renderApp(harness);
  await screen.findByText("Train pulls into Otaru station");
  return booted;
}

function suggestions(container: HTMLElement) {
  const strip = container.querySelector<HTMLElement>(".ai-strip");
  return strip ? [...within(strip).queryAllByRole("button")].map((button) => button.textContent) : [];
}

afterEach(() => vi.restoreAllMocks());

describe("clip detail follows the selection", () => {
  it("never shows one clip's suggestions under another clip's title", async () => {
    const { container } = await bootApp();
    await waitFor(() => expect(suggestions(container)).toContain("station"));

    // Synchronous on purpose: the read for the newly selected clip has not
    // resolved yet, which is exactly the moment the stale chips used to show.
    fireEvent.click(screen.getByText("DJI_001.MP4"));
    expect(suggestions(container)).toEqual([]);

    await waitFor(() => expect(screen.getByLabelText("Clip title")).toHaveValue(""));
    expect(suggestions(container)).toEqual([]);
  });

  it("brings them back on returning to the clip that has them", async () => {
    const { container } = await bootApp();
    fireEvent.click(screen.getByText("DJI_001.MP4"));
    fireEvent.click(screen.getByText("Train pulls into Otaru station"));
    await waitFor(() => expect(suggestions(container)).toContain("station"));
  });
});

describe("clip detail is not refetched by unrelated asset writes", () => {
  it("does not read the observation again on every keystroke in the notes field", async () => {
    await bootApp();
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
    await bootApp();
    const analysis = vi.spyOn(api, "assetAnalysis");
    fireEvent.click(screen.getByText("DJI_001.MP4"));
    await waitFor(() => expect(analysis).toHaveBeenCalledWith(UNOBSERVED));
  });
});
