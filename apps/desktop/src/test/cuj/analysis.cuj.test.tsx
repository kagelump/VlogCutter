/**
 * CUJ 6 and 7: analysing a project, and stopping partway.
 *
 * No model is downloaded, no key is stored, no request is made, and no test
 * waits a millisecond of real time. What is under test is the gate, the stage
 * progress, the money, and — the case worth the whole file — that a cancelled
 * run cannot be made to look like a finished one.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, renderApp, type AnalysisOutcome, type AnalysisStep } from "../harness";

function project() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 4, withProxy: true, startAt: "2026-02-08T00:10:00.000Z" });
}

const clips = [
  "asset_sony_a7c_ii_001",
  "asset_sony_a7c_ii_002",
  "asset_sony_a7c_ii_003",
  "asset_sony_a7c_ii_004",
];

async function openAnalysis(app: Awaited<ReturnType<typeof renderApp>>) {
  await app.user.click(screen.getByLabelText("More media generation actions"));
  await app.user.click(await screen.findByRole("button", { name: /Analyze \d+ clips/ }));
  return screen.findByRole("dialog", { name: /Hokkaido field test/ });
}

describe("CUJ 6 — analysing a project", () => {
  it("refuses to start until the speech models are installed", async () => {
    const harness = project().modelsInstalled(false).build();
    const app = await renderApp(harness);

    const dialog = await openAnalysis(app);
    expect(within(dialog).getByText(/Speech models are not installed/)).toBeVisible();
    expect(within(dialog).getByRole("button", { name: "Start" })).toBeDisabled();

    // Installing them from Settings is the path the dialog offers.
    await app.user.click(within(dialog).getByRole("button", { name: /Install them in Settings/ }));
    await app.user.click(await screen.findByRole("button", { name: "Install" }));
    await waitFor(() => expect(harness.state.models.every((model) => model.installed)).toBe(true));

    await app.user.keyboard("{Escape}");
    const reopened = await openAnalysis(app);
    expect(within(reopened).queryByText(/Speech models are not installed/)).toBeNull();
    expect(within(reopened).getByRole("button", { name: "Start" })).toBeEnabled();
  });

  it("refuses to start until there is an API key, and takes one", async () => {
    const harness = project().apiKeyPresent(false).build();
    const app = await renderApp(harness);

    const dialog = await openAnalysis(app);
    expect(within(dialog).getByText(/No OpenRouter key/)).toBeVisible();
    expect(within(dialog).getByRole("button", { name: "Start" })).toBeDisabled();

    await app.user.click(within(dialog).getByRole("button", { name: /Add one in Settings/ }));
    await app.user.type(await screen.findByLabelText(/OpenRouter/i), "sk-or-harness");
    await app.user.click(screen.getByRole("button", { name: /^Save/ }));
    await waitFor(() => expect(harness.state.openRouterKeyPresent).toBe(true));
    expect(harness.state.openRouterKey).toBe("sk-or-harness");

    await app.user.keyboard("{Escape}");
    const reopened = await openAnalysis(app);
    expect(within(reopened).getByRole("button", { name: "Start" })).toBeEnabled();
  });

  it("plans the work from what the project is actually missing", async () => {
    const harness = project()
      .withTranscript(clips[0], { text: "Arriving now" })
      .withAnalysis(clips[0])
      .build();
    const app = await renderApp(harness);

    const dialog = await openAnalysis(app);
    const counts = within(dialog).getAllByRole("listitem").map((item) => item.querySelector("strong")?.textContent);
    // No proxies missing, one clip already transcribed, one already analysed.
    expect(counts).toEqual(["0", "3", "3"]);
    // Cost is derived from the clips that still need the cloud step. Three at
    // $0.0001084 rounds to the app's "too small to price" figure.
    expect(within(dialog).getByText("<$0.01")).toBeVisible();
    expect(within(dialog).getByText(/\$0\.000108 per clip/)).toBeVisible();
  });

  it("reports each stage as it happens and surfaces the results in the queue", async () => {
    const harness = project().build();
    const app = await renderApp(harness);
    const run = harness.job<AnalysisStep, AnalysisOutcome>("analyze_project");

    const dialog = await openAnalysis(app);
    await app.user.click(within(dialog).getByRole("button", { name: "Start" }));
    await run.started();

    await run.progress({ stage: "transcripts", assetId: clips[0], transcript: { text: "Next stop, Otaru" } });
    expect(await screen.findByText(/Transcribing · step 2 of 3/)).toBeVisible();

    await run.progress({ stage: "analysis", assetId: clips[0], analysis: { aiLabels: ["station"] } });
    await run.progress({ stage: "analysis", assetId: clips[1], analysis: { usable: false, unusableReason: "blurry" } });
    expect(await screen.findByText(/Analyzing clips · step 3 of 3/)).toBeVisible();

    await run.complete();

    // The results are in the project, not just in the run's report.
    await waitFor(() => expect(screen.getByText(/Analyzed 2 clips and transcribed 1/)).toBeVisible());
    expect(harness.state.analyses[clips[0]].aiLabels).toEqual(["station"]);
    expect(await screen.findByRole("button", { name: /Unusable 1/ })).toBeVisible();

    // Observed behavior, recorded rather than asserted as desirable: the
    // per-clip evidence read is keyed on the selection, so the clip the user is
    // sitting on keeps its empty rail until the selection moves. The queue
    // above already shows the verdict.
    expect(screen.queryByRole("button", { name: "station" })).toBeNull();

    await app.user.keyboard("jk");
    expect(await screen.findByRole("button", { name: "station" })).toBeVisible();
  });

  it("charges only for the clips it actually analysed", async () => {
    const harness = project().build();
    const app = await renderApp(harness);
    const run = harness.job<AnalysisStep, AnalysisOutcome>("analyze_project");

    const dialog = await openAnalysis(app);
    await app.user.click(within(dialog).getByRole("button", { name: "Start" }));
    await run.started();
    await run.progress({ stage: "analysis", assetId: clips[0] });
    await run.progress({ stage: "analysis", assetId: clips[1] });
    await run.complete();

    await waitFor(() => expect(harness.state.analysisRuns).toHaveLength(1));
    expect(harness.state.analysisRuns[0]).toMatchObject({
      status: "completed",
      clipsAnalyzed: 2,
      costUsd: 0.0002168,
    });
  });

  it("records the run against the harness clock", async () => {
    const harness = project().build();
    const app = await renderApp(harness);
    harness.clock.set("2026-08-09T14:30:00.000Z");

    const dialog = await openAnalysis(app);
    await app.user.click(within(dialog).getByRole("button", { name: "Start" }));

    await waitFor(() => expect(harness.state.analysisRuns).toHaveLength(1));
    expect(harness.state.analysisRuns[0].finishedAt).toBe("2026-08-09T14:30:00.000Z");
  });
});

describe("CUJ 7 — stopping a run", () => {
  it("cannot be made to show a completion it did not reach", async () => {
    const harness = project().build();
    const app = await renderApp(harness);
    const run = harness.job<AnalysisStep, AnalysisOutcome>("analyze_project");

    const dialog = await openAnalysis(app);
    await app.user.click(within(dialog).getByRole("button", { name: "Start" }));
    await run.started();

    await run.progress({ stage: "transcripts", assetId: clips[0] });
    await run.progress({ stage: "transcripts", assetId: clips[1] });

    // The user presses Cancel: the command only asks the run to stop.
    await app.user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(harness.calls("cancel_analysis")).toHaveLength(1);
    // The progress readout is still on screen: the run has not stopped yet, and
    // the app does not pretend otherwise.
    expect(screen.getByRole("progressbar")).toBeVisible();

    // The run then finishes on its own terms, with what it had reached.
    await run.cancel();

    await waitFor(() => expect(screen.getByText(/Analysis stopped during transcribing/)).toBeVisible());
    expect(screen.getByText(/0 clips analyzed, \$0\.00 spent/)).toBeVisible();
    expect(harness.state.analysisRuns[0]).toMatchObject({ status: "cancelled", clipsAnalyzed: 0, costUsd: 0 });
    // Nothing was written for clips the run never reached.
    expect(harness.state.analyses[clips[0]]).toBeUndefined();
    expect(harness.state.transcripts[clips[0]]).toBeDefined();
  });

  it("keeps the work a cancelled run finished, and resumes on the next run", async () => {
    const harness = project().build();
    const app = await renderApp(harness);

    const first = harness.job<AnalysisStep, AnalysisOutcome>("analyze_project");
    let dialog = await openAnalysis(app);
    await app.user.click(within(dialog).getByRole("button", { name: "Start" }));
    await first.started();
    await first.progress({ stage: "analysis", assetId: clips[0] });
    await app.user.click(screen.getByRole("button", { name: "Cancel" }));
    await first.cancel();
    await waitFor(() => expect(screen.getByText(/Analysis stopped/)).toBeVisible());

    // The second run sees one clip fewer to do.
    dialog = await openAnalysis(app);
    const counts = within(dialog).getAllByRole("listitem").map((item) => item.querySelector("strong")?.textContent);
    expect(counts[2]).toBe("3");

    await app.user.click(within(dialog).getByRole("button", { name: "Start" }));
    await waitFor(() => expect(screen.getByText(/Analyzed 3 clips/)).toBeVisible());
    expect(Object.keys(harness.state.analyses)).toHaveLength(4);
    expect(harness.state.analysisRuns.map((run) => run.status)).toEqual(["cancelled", "completed"]);
  });

  it("says what it kept when the budget stops it", async () => {
    const harness = project().build();
    const app = await renderApp(harness);
    const run = harness.job<AnalysisStep, AnalysisOutcome>("analyze_project");

    const dialog = await openAnalysis(app);
    await app.user.type(within(dialog).getByLabelText(/Stop this run at/), "0.25");
    await app.user.click(within(dialog).getByRole("button", { name: "Start" }));
    await run.started();

    await run.progress({ stage: "analysis", assetId: clips[0] });
    await run.complete({ budgetExhausted: true });

    expect(await screen.findByText(/Stopped at the .* budget for this run/)).toBeVisible();
    expect(harness.state.analyses[clips[0]]).toBeDefined();
  });

  it("reports a backend failure without claiming a result", async () => {
    const harness = project().build();
    const app = await renderApp(harness);
    const run = harness.job<AnalysisStep, AnalysisOutcome>("analyze_project");

    const dialog = await openAnalysis(app);
    await app.user.click(within(dialog).getByRole("button", { name: "Start" }));
    await run.started();
    await run.progress({ stage: "transcripts", assetId: clips[0] });
    await run.fail("whisper.cpp exited with status 1");

    expect(await screen.findByText(/whisper\.cpp exited with status 1/)).toBeVisible();
    expect(harness.state.analysisRuns).toHaveLength(0);
    expect(screen.queryByRole("progressbar")).toBeNull();
  });
});
