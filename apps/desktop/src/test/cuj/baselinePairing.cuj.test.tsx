/**
 * CUJ: pairing a project with an already-connected server.
 *
 * The baseline wizard is a five-step state machine — map roots, preview,
 * upload, done, or recover — and none of its transitions had journey coverage:
 * `handleStartBaselineWizard`, `handleProceedToPreview`, `handleConfirmBaseline`,
 * `handleRetryBaseline` and `handleCancelBaselineWizard` were all unreached, as
 * were the six `api` calls behind them. `BaselineWizardModal` had component
 * tests for its rendering, but nothing drove the shell that owns the steps.
 *
 * Two promises this flow makes in its own copy are asserted here, because both
 * are the kind a refactor breaks without any visible symptom:
 *
 *  - no original media is uploaded — the whole flow is metadata;
 *  - a failed upload loses nothing and can be retried.
 *
 * The digest guard gets its own case: it is the only thing standing between a
 * user who edited the project while reading the preview and a baseline that
 * describes something else.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, renderApp } from "../harness";

const PROJECT = "project_hokkaido";

/** A project on a machine already connected to a server, not yet paired itself. */
function connectedToServer() {
  return desktopScenario({ now: "2026-08-14T09:00:00.000Z" })
    .project("Hokkaido field test", { id: PROJECT })
    .camera("Sony A7C II", { clips: 2 })
    .camera("DJI Pocket 3", { clips: 1 })
    .paired({ label: "Studio server" })
    .build();
}

async function openWizard(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(await screen.findByRole("button", { name: "Pair with server" }));
  return screen.findByRole("dialog", { name: /Step 1: Map media paths/ });
}

async function reachPreview(app: Awaited<ReturnType<typeof renderApp>>) {
  const modal = await openWizard(app);
  await app.user.click(within(modal).getByRole("button", { name: "Preview baseline" }));
  await screen.findByRole("dialog", { name: /Step 2: Review baseline preview/ });
  return modal;
}

describe("starting the baseline wizard", () => {
  it("is offered only once a server connection exists", async () => {
    const unpaired = desktopScenario({ now: "2026-08-14T09:00:00.000Z" })
      .project("Hokkaido field test", { id: PROJECT })
      .camera("Sony A7C II", { clips: 2 })
      .build();
    await renderApp(unpaired);

    expect(screen.queryByRole("button", { name: "Pair with server" })).toBeNull();
  });

  it("analyses the project's paths and proposes a root for each one", async () => {
    const harness = connectedToServer();
    const app = await renderApp(harness);

    const modal = await openWizard(app);

    expect(harness.calls("analyze_project_paths")).toEqual([{ projectId: PROJECT }]);
    // The proposal is derived from the analysis the backend returned, so the
    // analysis has to be handed back rather than recomputed on the way.
    expect(harness.calls("propose_root_mappings")[0]).toMatchObject({
      pathAnalysis: { totalAssets: 3, absolutePathCount: 3 },
    });
    expect(within(modal).getByText(/assets with absolute paths/)).toHaveTextContent("3");
    expect(within(modal).getByText(/No original media will be uploaded/)).toBeVisible();
  });

  it("reports a failed analysis inside the wizard, not as a dead end", async () => {
    const harness = connectedToServer();
    harness.faults.always("analyze_project_paths", {
      kind: "reject",
      message: "project database is busy",
    });
    const app = await renderApp(harness);

    const modal = await openWizard(app);

    expect(within(modal).getByRole("alert")).toHaveTextContent(/Failed to analyze project paths/);
    // Still on step one, so Cancel is reachable and nothing was sent onward.
    expect(harness.commandNames()).not.toContain("preview_project_baseline");
  });
});

describe("previewing the baseline", () => {
  it("shows what the server would receive, having sent nothing yet", async () => {
    const harness = connectedToServer();
    const app = await renderApp(harness);

    const modal = await reachPreview(app);

    expect(harness.calls("preview_project_baseline")).toEqual([{ projectId: PROJECT }]);
    expect(harness.commandNames()).not.toContain("confirm_baseline_import");
    expect(within(modal).getByText("Hokkaido field test")).toBeVisible();
    const assets = within(modal).getByText("Assets").previousElementSibling;
    expect(assets).toHaveTextContent("3");
    expect(within(modal).getByText(/will NOT upload any original media files/)).toBeVisible();
  });

  it("goes back to the mapping step without discarding the analysis", async () => {
    const harness = connectedToServer();
    const app = await renderApp(harness);
    const modal = await reachPreview(app);
    harness.clearLog();

    await app.user.click(within(modal).getByRole("button", { name: "Back" }));

    await screen.findByRole("dialog", { name: /Step 1: Map media paths/ });
    // Back is a step change, not a restart: re-analysing here would throw away
    // mappings the user may have been reviewing.
    expect(harness.commandNames()).not.toContain("analyze_project_paths");
    expect(within(modal).getByText(/assets with absolute paths/)).toBeVisible();
  });

  it("returns to mapping and says why when the preview fails", async () => {
    const harness = connectedToServer();
    harness.faults.always("preview_project_baseline", {
      kind: "reject",
      message: "server is unreachable",
    });
    const app = await renderApp(harness);
    const modal = await openWizard(app);

    await app.user.click(within(modal).getByRole("button", { name: "Preview baseline" }));

    expect(await within(modal).findByRole("alert")).toHaveTextContent(/Preview failed: .*server is unreachable/);
    expect(await screen.findByRole("dialog", { name: /Step 1: Map media paths/ })).toBeVisible();
  });
});

describe("uploading the baseline", () => {
  it("uploads the previewed digest and reports the project paired", async () => {
    const harness = connectedToServer();
    const app = await renderApp(harness);
    const modal = await reachPreview(app);

    await app.user.click(within(modal).getByRole("button", { name: /Confirm & upload baseline/ }));

    await screen.findByRole("dialog", { name: /Project paired!/ });
    // The digest sent is the one the user was shown, which is what makes the
    // server's staleness check mean anything.
    expect(harness.calls("confirm_baseline_import")).toEqual([
      { projectId: PROJECT, expectedDigest: harness.state.pairing.baselineDigest },
    ]);
    expect(harness.state.projects[0].pairedServerNodeId).toBe(harness.state.pairing.serverInfo.nodeId);
  });

  it("closes cleanly on Done, leaving no wizard state behind", async () => {
    const harness = connectedToServer();
    const app = await renderApp(harness);
    const modal = await reachPreview(app);
    await app.user.click(within(modal).getByRole("button", { name: /Confirm & upload baseline/ }));
    const complete = await screen.findByRole("dialog", { name: /Project paired!/ });

    await app.user.click(within(complete).getByRole("button", { name: "Done" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    // Reopening starts at step one with a fresh analysis rather than showing
    // the previous run's preview.
    const reopened = await openWizard(app);
    expect(within(reopened).queryByText(/Baseline digest/)).toBeNull();
  });

  it("refuses a digest the project has moved past, and says to preview again", async () => {
    const harness = connectedToServer();
    const app = await renderApp(harness);
    const modal = await reachPreview(app);
    // The project changed while the preview was on screen.
    harness.state.pairing.baselineDigest = "digest_after_the_edit";

    await app.user.click(within(modal).getByRole("button", { name: /Confirm & upload baseline/ }));

    const failed = await screen.findByRole("dialog", { name: /Pairing issue/ });
    expect(within(failed).getByRole("alert")).toHaveTextContent(/changed while you were reviewing/);
    expect(harness.state.projects[0].pairedServerNodeId ?? null).toBeNull();
  });
});

describe("recovering from a failed upload", () => {
  it("offers a retry that says nothing was lost, and completes on the second attempt", async () => {
    const harness = connectedToServer();
    harness.faults.queue("confirm_baseline_import", { kind: "reject", message: "connection reset" });
    const app = await renderApp(harness);
    const modal = await reachPreview(app);

    await app.user.click(within(modal).getByRole("button", { name: /Confirm & upload baseline/ }));
    const failed = await screen.findByRole("dialog", { name: /Pairing issue/ });
    expect(within(failed).getByText(/no data has been lost/)).toBeVisible();

    // The one-shot fault is spent; the retry re-previews and then uploads.
    await app.user.click(within(failed).getByRole("button", { name: "Retry" }));
    await app.user.click(
      within(await screen.findByRole("dialog", { name: /Step 2/ })).getByRole("button", {
        name: /Confirm & upload baseline/,
      }),
    );

    await screen.findByRole("dialog", { name: /Project paired!/ });
    expect(harness.calls("confirm_baseline_import")).toHaveLength(2);
    expect(harness.state.projects[0].pairedServerNodeId).toBe(harness.state.pairing.serverInfo.nodeId);
  });

  it("re-previews on retry rather than reusing the digest that just failed", async () => {
    const harness = connectedToServer();
    harness.faults.queue("confirm_baseline_import", { kind: "reject", message: "connection reset" });
    const app = await renderApp(harness);
    const modal = await reachPreview(app);
    await app.user.click(within(modal).getByRole("button", { name: /Confirm & upload baseline/ }));
    const failed = await screen.findByRole("dialog", { name: /Pairing issue/ });
    harness.clearLog();

    await app.user.click(within(failed).getByRole("button", { name: "Retry" }));

    await waitFor(() => expect(harness.calls("preview_project_baseline")).toHaveLength(1));
  });

  it("abandons the server-side attempt when the wizard is cancelled", async () => {
    const harness = connectedToServer();
    const app = await renderApp(harness);
    const modal = await reachPreview(app);

    await app.user.click(within(modal).getByRole("button", { name: /^Cancel/ }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    // A half-finished baseline left on the server is the thing this call
    // exists to clear, so cancelling must reach it rather than just closing.
    expect(harness.commandNames()).toContain("abandon_baseline_attempt");
    expect(harness.state.projects[0].pairedServerNodeId ?? null).toBeNull();
  });

  it("closes even when abandoning fails on the server", async () => {
    const harness = connectedToServer();
    harness.faults.always("abandon_baseline_attempt", { kind: "reject", message: "server is gone" });
    const app = await renderApp(harness);
    const modal = await reachPreview(app);

    await app.user.click(within(modal).getByRole("button", { name: /^Cancel/ }));

    // The user asked to stop. A failure to tell the server that must not trap
    // them in the wizard or raise an error they can do nothing about.
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
