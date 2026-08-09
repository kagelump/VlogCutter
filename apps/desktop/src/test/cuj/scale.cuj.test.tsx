/**
 * CUJ 14: a project the size of a real trip.
 *
 * Six hundred clips across four cameras, with no media files anywhere. This is
 * about ordering, filtering, selection, and navigation staying correct and
 * usable at a size the demo fixtures never reach — the failures here are
 * off-by-one boundaries and refetch storms, not visual ones.
 */

import { screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, queueFilenames, renderApp, selectedRow } from "../harness";

// Rendering six hundred rows costs more than the default per-test budget, and
// the point of the case is that it stays correct at size.
const SCALE_TIMEOUT = 30_000;

function expedition() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Antarctica 2026", { id: "project_antarctica" })
    .camera("Sony A7C II", { clips: 200, withProxy: true, startAt: "2026-02-01T08:00:00.000Z", spacingSeconds: 600 })
    .camera("DJI Pocket 3", { clips: 200, withProxy: true, startAt: "2026-02-01T08:02:00.000Z", spacingSeconds: 600 })
    .camera("Insta360 X5", { clips: 100, startAt: "2026-02-01T08:04:00.000Z", spacingSeconds: 1_200 })
    .camera("Apple iPhone 17 Pro", { clips: 100, startAt: "2026-02-01T08:06:00.000Z", spacingSeconds: 1_200 })
    .build();
}

describe("CUJ 14 — a six-hundred-clip project", () => {
  it(
    "opens with every clip in the queue, interleaved by capture time",
    async () => {
      const harness = expedition();
      await renderApp(harness);

      const filenames = queueFilenames();
      expect(filenames).toHaveLength(600);
      // Four cameras minutes apart: the queue is the trip, not four folders.
      expect(filenames.slice(0, 4)).toEqual([
        "SONY_001.MP4",
        "DJI_001.MP4",
        "INSTA360_001.MP4",
        "APPLE_001.MP4",
      ]);
      expect(selectedRow()?.textContent).toContain("SONY_001.MP4");
    },
    SCALE_TIMEOUT,
  );

  it(
    "steps to the end of the queue and stops there rather than losing the selection",
    async () => {
      const harness = expedition();
      const app = await renderApp(harness);
      const last = queueFilenames().at(-1)!;

      await app.user.click(screen.getByText(last));
      await waitFor(() => expect(selectedRow()?.textContent).toContain(last));

      await app.user.keyboard("jjj");
      expect(selectedRow()?.textContent).toContain(last);
    },
    SCALE_TIMEOUT,
  );

  it(
    "holds the boundary at the top of the queue too",
    async () => {
      const harness = expedition();
      const app = await renderApp(harness);

      await app.user.keyboard("kkk");
      expect(selectedRow()?.textContent).toContain("SONY_001.MP4");
    },
    SCALE_TIMEOUT,
  );

  it(
    "filters six hundred clips down to one camera's worth",
    async () => {
      const harness = expedition();
      const app = await renderApp(harness);

      await app.user.type(screen.getByPlaceholderText(/Filter titles/), "insta360");
      await waitFor(() => expect(queueFilenames()).toHaveLength(100));
    },
    SCALE_TIMEOUT,
  );

  it(
    "writes a judgment on a clip deep in the queue",
    async () => {
      const harness = expedition();
      const app = await renderApp(harness);

      await app.user.type(screen.getByPlaceholderText(/Filter titles/), "SONY_150");
      await waitFor(() => expect(queueFilenames()).toEqual(["SONY_150.MP4"]));

      await app.user.type(screen.getByLabelText("Add label"), "iceberg{Enter}");
      await waitFor(() => expect(harness.state.assets.asset_sony_a7c_ii_150.labels).toEqual(["iceberg"]));
    },
    SCALE_TIMEOUT,
  );

  it(
    "reads the project once on open, not once per clip",
    async () => {
      const harness = expedition();
      await renderApp(harness);

      // The guard is against a refetch storm keyed on the asset list; six
      // hundred clips makes that mistake unmissable rather than merely slow.
      expect(harness.calls("list_assets")).toHaveLength(1);
      expect(harness.calls("project_analysis_index")).toHaveLength(1);
      expect(harness.calls("asset_transcript")).toHaveLength(1);
    },
    SCALE_TIMEOUT,
  );
});
