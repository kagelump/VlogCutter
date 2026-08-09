/**
 * The edit-plan lifecycle.
 *
 * Validation findings, preview results, and export digests are declared by the
 * scenario — whether a plan *is* valid is `rcdvlog-core`'s judgment, and the
 * OTIO file's contents are proven by its own Rust tests. What is under test
 * here is the lifecycle the frontend depends on: one active revision, a write
 * that carries the revision it believed was active, a blocking finding that
 * blocks, and an export that refuses to silently replace a file.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, renderApp, type PreviewStep } from "../harness";

function project() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 3, withProxy: true, durationSeconds: 12, startAt: "2026-02-08T00:10:00.000Z" });
}

async function enterEditPlan(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(screen.getByRole("button", { name: "Edit plan" }));
  return screen.findByRole("region", { name: "Edit plan" });
}

describe("building an edit plan", () => {
  it("seeds a plan from the review queue, in the queue's order", async () => {
    const harness = project().build();
    const app = await renderApp(harness);
    const panel = await enterEditPlan(app);

    await app.user.click(within(panel).getByRole("button", { name: /Build one from the review queue/ }));

    await waitFor(() => expect(harness.state.editPlans.active).not.toBeNull());
    expect(harness.state.editPlans.active!.operations.map((operation) => operation.assetId)).toEqual([
      "asset_sony_a7c_ii_001",
      "asset_sony_a7c_ii_002",
      "asset_sony_a7c_ii_003",
    ]);
    expect(await within(panel).findByText(/Built a plan from 3 clips/)).toBeVisible();
  });

  it("leaves removed clips out of the plan", async () => {
    const harness = project().removed("asset_sony_a7c_ii_002").build();
    const app = await renderApp(harness);
    const panel = await enterEditPlan(app);

    await app.user.click(within(panel).getByRole("button", { name: /Build one from the review queue/ }));

    await waitFor(() => expect(harness.state.editPlans.active?.operations).toHaveLength(2));
    expect(harness.state.editPlans.active!.operations.map((operation) => operation.assetId)).toEqual([
      "asset_sony_a7c_ii_001",
      "asset_sony_a7c_ii_003",
    ]);
  });

  it("names the revision it believed was active on every write", async () => {
    const harness = project().build();
    const app = await renderApp(harness);
    const panel = await enterEditPlan(app);

    await app.user.click(within(panel).getByRole("button", { name: /Build one from the review queue/ }));
    await waitFor(() => expect(harness.state.editPlans.active).not.toBeNull());

    // First build: nothing was active, so the expectation is null.
    expect(harness.calls("create_edit_plan_revision")[0]).toMatchObject({ expectedActiveRevisionId: null });
    expect(harness.calls("activate_edit_plan_revision")[0]).toMatchObject({
      revisionId: "revision_1",
      expectedActiveRevisionId: null,
    });

    // Rebuilding carries the revision the user was looking at.
    await app.user.click(within(panel).getByRole("button", { name: /Rebuild/ }));
    await waitFor(() => expect(harness.state.editPlans.revisions).toHaveLength(2));
    expect(harness.calls("create_edit_plan_revision")[1]).toMatchObject({ expectedActiveRevisionId: "revision_1" });
    expect(harness.state.editPlans.active?.revisionId).toBe("revision_2");
  });

  it("refuses a write against a plan that changed underneath, and writes nothing", async () => {
    const harness = project().editPlanConflict("The edit plan changed since you loaded it").build();
    const app = await renderApp(harness);
    const panel = await enterEditPlan(app);

    await app.user.click(within(panel).getByRole("button", { name: /Build one from the review queue/ }));

    expect(await within(panel).findByText(/changed since you loaded it/)).toBeVisible();
    expect(harness.state.editPlans.revisions).toHaveLength(0);
    expect(harness.state.editPlans.active).toBeNull();
    expect(harness.commandNames()).not.toContain("activate_edit_plan_revision");
  });
});

describe("validation and export", () => {
  it("blocks preview and export while a finding is an error", async () => {
    const harness = project()
      .editPlanValidation("revision_1", {
        findings: [
          {
            kind: "media",
            severity: "error",
            code: "missing_original",
            position: 1,
            assetId: "asset_sony_a7c_ii_002",
            message: "SONY_002.MP4 has no reachable media",
          },
        ],
      })
      .build();
    const app = await renderApp(harness);
    const panel = await enterEditPlan(app);

    await app.user.click(within(panel).getByRole("button", { name: /Build one from the review queue/ }));

    expect(await within(panel).findByText(/1 problem\(s\) block this plan/)).toBeVisible();
    expect(within(panel).getByText(/no reachable media/)).toBeVisible();
    expect(within(panel).getByRole("button", { name: "Render preview" })).toBeDisabled();
    expect(within(panel).getByRole("button", { name: "Export OTIO" })).toBeDisabled();
  });

  it("shows a warning without blocking the work", async () => {
    const harness = project()
      .editPlanValidation("revision_1", {
        findings: [
          {
            kind: "pacing",
            severity: "warning",
            code: "long_clip",
            position: 0,
            assetId: "asset_sony_a7c_ii_001",
            message: "This clip runs longer than the rest put together",
          },
        ],
      })
      .build();
    const app = await renderApp(harness);
    const panel = await enterEditPlan(app);

    await app.user.click(within(panel).getByRole("button", { name: /Build one from the review queue/ }));

    expect(await within(panel).findByText(/1 thing\(s\) worth knowing/)).toBeVisible();
    expect(within(panel).getByRole("button", { name: "Render preview" })).toBeEnabled();
  });

  it("reports preview progress as the render emits it", async () => {
    const harness = project().build();
    const app = await renderApp(harness);
    const panel = await enterEditPlan(app);
    await app.user.click(within(panel).getByRole("button", { name: /Build one from the review queue/ }));
    await within(panel).findByText(/Built a plan from 3 clips/);

    const render = harness.job<PreviewStep, { failures?: never[] }>("render_edit_plan_preview");
    await app.user.click(within(panel).getByRole("button", { name: "Render preview" }));
    await render.started();

    await render.progress({ position: 1 });
    expect(await within(panel).findByText(/Rendering clip 1/)).toBeVisible();

    await render.progress({ position: 2 });
    await render.complete();

    expect(await within(panel).findByText(/Preview rendered 3 clips/)).toBeVisible();
  });

  it("exports the active revision to the path the user typed", async () => {
    const harness = project().build();
    const app = await renderApp(harness);
    const panel = await enterEditPlan(app);
    await app.user.click(within(panel).getByRole("button", { name: /Build one from the review queue/ }));
    await within(panel).findByText(/Built a plan from 3 clips/);

    await app.user.type(within(panel).getByLabelText(/Export to/), "/Users/harness/Movies/trip.otio");
    await app.user.click(within(panel).getByRole("button", { name: "Export OTIO" }));

    expect(await within(panel).findByText(/Exported 3 clips to \/Users\/harness\/Movies\/trip\.otio/)).toBeVisible();
    expect(harness.calls("export_edit_plan_otio")[0]).toMatchObject({
      revisionId: "revision_1",
      outputPath: "/Users/harness/Movies/trip.otio",
      timelineName: "Trip",
      expectedExistingDigest: null,
    });
  });

  it("refuses to replace an existing export without being told to", async () => {
    const harness = project()
      .existingExport({
        outputPath: "/Users/harness/Movies/trip.otio",
        outputDigest: "digest_from_last_week",
        clipCount: 9,
      })
      .build();
    const app = await renderApp(harness);
    const panel = await enterEditPlan(app);
    await app.user.click(within(panel).getByRole("button", { name: /Build one from the review queue/ }));
    await within(panel).findByText(/Built a plan from 3 clips/);

    await app.user.type(within(panel).getByLabelText(/Export to/), "/Users/harness/Movies/trip.otio");
    await app.user.click(within(panel).getByRole("button", { name: "Export OTIO" }));

    // The refusal names the digest actually on disk, and the file is untouched.
    expect(await within(panel).findByText(/already exists with digest digest_from_last_week/)).toBeVisible();
    expect(harness.state.editPlans.exportResult?.clipCount).toBe(9);
  });
});
