/**
 * CUJ: something else writes to a clip while the user is reviewing it.
 *
 * The scenario this exists for is an agent annotating the queue through the
 * CLI while the desktop is open — the workflow issue #47 is heading towards,
 * and the one this guard had to land before.
 *
 * A judgment is written as a whole `AssetPatch`: notes, labels, removal and
 * rotation travel together. So a patch built from a read that has since gone
 * stale does not merely fail to include the other writer's label — it carries
 * the old label list and *removes* it. The catalog now refuses such a write,
 * and the desktop's answer is to take what the catalog holds, put the user's
 * own edit back on top, and send that.
 */

import { screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, renderApp } from "../harness";

const first = "asset_sony_a7c_ii_001";

function session() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 2, withProxy: true })
    .build();
}

/** Somebody else's write, landing between the desktop's read and its write. */
function writeElsewhere(harness: ReturnType<typeof session>, changes: Record<string, unknown>) {
  const current = harness.state.assets[first];
  harness.state.assets[first] = { ...current, ...changes, revision: current.revision + 1 };
}

describe("a clip that changed under the desktop", () => {
  it("keeps the other writer's label instead of removing it", async () => {
    const harness = session();
    const app = await renderApp(harness);
    const notes = screen.getByLabelText("Notes");
    await app.user.type(notes, "The station clock is legible");

    writeElsewhere(harness, { labels: ["agent-added"] });
    await app.user.click(screen.getByText("SONY_002.MP4"));

    await waitFor(() => expect(harness.state.assets[first].notes).toBe("The station clock is legible"));
    // Both survive. Before the revision check the whole patch would have gone
    // through carrying `labels: []`, and the label would have been gone with
    // nothing anywhere reporting it.
    expect(harness.state.assets[first].labels).toEqual(["agent-added"]);
  });

  it("refuses the first write, then sends the same edit at the new revision", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await app.user.type(screen.getByLabelText("Notes"), "typed while the agent wrote");

    writeElsewhere(harness, { labels: ["agent-added"] });
    await app.user.click(screen.getByText("SONY_002.MP4"));

    await waitFor(() => expect(harness.calls("update_asset")).toHaveLength(2));
    const [refused, accepted] = harness.calls("update_asset").map((call) => call.patch as Record<string, unknown>);
    expect(refused.expectedRevision).toBe(0);
    expect(refused.labels).toEqual([]);
    // The recovery reads the clip again and rebuilds the patch on what it says.
    expect(harness.commandNames()).toContain("get_asset");
    expect(accepted.expectedRevision).toBe(1);
    expect(accepted.labels).toEqual(["agent-added"]);
  });

  it("shows the other writer's change in the queue afterwards", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await app.user.type(screen.getByLabelText("Notes"), "typed while the agent wrote");

    writeElsewhere(harness, { labels: ["agent-added"] });
    await app.user.click(screen.getByText("SONY_002.MP4"));
    await waitFor(() => expect(harness.state.assets[first].labels).toEqual(["agent-added"]));

    await app.user.click(screen.getByText("SONY_001.MP4"));

    // Not only in the catalog: the reviewer finds out, on the clip they were
    // looking at, without reloading anything.
    await waitFor(() => expect(screen.getByLabelText("Clip labels").textContent).toContain("agent-added"));
    expect(screen.getByLabelText("Notes")).toHaveValue("typed while the agent wrote");
  });

  it("does nothing extra when nothing else wrote", async () => {
    const harness = session();
    const app = await renderApp(harness);

    await app.user.type(screen.getByLabelText("Notes"), "an ordinary note");
    await app.user.click(screen.getByText("SONY_002.MP4"));

    await waitFor(() => expect(harness.state.assets[first].notes).toBe("an ordinary note"));
    expect(harness.calls("update_asset")).toHaveLength(1);
    expect(harness.commandNames()).not.toContain("get_asset");
  });

  it("keeps the paragraph on screen and retryable when the recovery also fails", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await app.user.type(screen.getByLabelText("Notes"), "worth keeping");
    // Every write is refused: a writer that keeps winning the race.
    harness.always("update_asset").reject("stale asset revision: expected revision 0, catalog is at 7");

    await app.user.click(screen.getByText("SONY_002.MP4"));

    expect(await screen.findByText(/stale asset revision/)).toBeVisible();
    await app.user.click(screen.getByText("SONY_001.MP4"));
    // Human prose is not thrown away because two writers collided.
    await waitFor(() => expect(screen.getByLabelText("Notes")).toHaveValue("worth keeping"));
    expect(await screen.findByText("Unsaved")).toBeVisible();
  });
});
