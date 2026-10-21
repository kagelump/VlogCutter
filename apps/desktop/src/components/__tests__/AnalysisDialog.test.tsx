import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { AnalysisDialog } from "../AnalysisDialog";
import type { AnalysisReadiness } from "../../types";

function readiness(overrides: Partial<AnalysisReadiness> = {}): AnalysisReadiness {
  return {
    projectId: "project_1",
    totalClips: 5,
    reachableOriginals: 5,
    unverifiedVideoClips: 0,
    quarantinedVideoClips: 0,
    unavailableVideoClips: 0,
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
    primaryModel: "qwen/qwen3.7-flash",
    allowedModels: ["qwen/qwen3.7-flash", "google/gemini-2.5-flash-lite"],
    allowFallback: true,
    onBudgetChange: vi.fn(),
    onForceReanalyzeChange: vi.fn(),
    onPrimaryModelChange: vi.fn(),
    onAllowedModelsChange: vi.fn(),
    onAllowFallbackChange: vi.fn(),
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

  it("explains reachable originals that still need validation or are quarantined", () => {
    renderDialog({ totalClips: 2, reachableOriginals: 4, unverifiedVideoClips: 1, quarantinedVideoClips: 1 });
    expect(screen.getByRole("status")).toHaveTextContent("4 original files are reachable");
    expect(screen.getByRole("status")).toHaveTextContent("1 clip needs validation");
    expect(screen.getByRole("status")).toHaveTextContent("1 clip is quarantined");
    expect(screen.getByRole("button", { name: "Start" })).toBeEnabled();
  });
});
