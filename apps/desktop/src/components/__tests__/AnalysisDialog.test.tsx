import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { AnalysisDialog } from "../AnalysisDialog";
import type { AnalysisReadiness } from "../../types";

function readiness(overrides: Partial<AnalysisReadiness> = {}): AnalysisReadiness {
  return {
    projectId: "project_1",
    totalClips: 5,
    missingProxies: 1,
    missingTranscripts: 2,
    missingAnalyses: 3,
    models: [],
    modelsReady: true,
    apiKeyPresent: true,
    estimatedCostUsd: 0.12,
    costBasis: "reference",
    observedCostPerClipUsd: 0.02,
    lastRun: null,
    ...overrides,
  };
}

function renderDialog(overrides: Partial<AnalysisReadiness> = {}) {
  const props = {
    projectName: "Hokkaido",
    readiness: readiness(overrides),
    budget: "1.00",
    forceReanalyze: false,
    onBudgetChange: vi.fn(),
    onForceReanalyzeChange: vi.fn(),
    onOpenSettings: vi.fn(),
    onCancel: vi.fn(),
    onStart: vi.fn(),
  };
  return { props, ...render(<AnalysisDialog {...props} />) };
}

describe("AnalysisDialog", () => {
  it("starts the run when the project is ready", async () => {
    const { props } = renderDialog();
    const start = screen.getByRole("button", { name: "Start" });
    expect(start).toBeEnabled();
    await userEvent.click(start);
    expect(props.onStart).toHaveBeenCalledTimes(1);
  });

  it("blocks Start and routes to Settings when the API key is missing", async () => {
    const { props } = renderDialog({ apiKeyPresent: false });
    expect(screen.getByRole("button", { name: "Start" })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: /add one in settings/i }));
    expect(props.onOpenSettings).toHaveBeenCalledTimes(1);
  });

  it("blocks Start when there is nothing to analyze", () => {
    renderDialog({ totalClips: 0 });
    expect(screen.getByRole("button", { name: "Start" })).toBeDisabled();
  });
});
