import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { BaselineWizardModal, type BaselineStep } from "../BaselineWizardModal";
import type { BaselinePreviewResult, PathAnalysis } from "../../types";

const pathAnalysis: PathAnalysis = {
  commonPrefixes: [{ prefix: "/Volumes/Card", assetCount: 12, sampleAssets: [] }],
  totalAssets: 12,
  absolutePathCount: 12,
};

const preview: BaselinePreviewResult = {
  digest: "abcdef0123456789deadbeef",
  endpointUsed: "https://mac.local",
  serverNodeId: "node_server",
  preview: {
    projectId: "p1", projectName: "Hokkaido", assetCount: 12, operationCount: 30, conflictCount: 0,
    linkedMediaCount: 12, proposedLibraryMatches: 0, rootMappingCount: 1, representationManifestCount: 2,
  },
};

function renderWizard(over: Partial<React.ComponentProps<typeof BaselineWizardModal>> = {}) {
  const props = {
    step: "root_mapping" as BaselineStep,
    error: null as string | null,
    pathAnalysis,
    rootMappings: [{ localPrefix: "/Volumes/Card", storageRootId: "root_card", kind: "removable", label: "Card" }],
    preview,
    busy: false,
    onPreview: vi.fn(),
    onBack: vi.fn(),
    onConfirm: vi.fn(),
    onRetry: vi.fn(),
    onDone: vi.fn(),
    onCancel: vi.fn(),
    ...over,
  };
  return { props, ...render(<BaselineWizardModal {...props} />) };
}

describe("BaselineWizardModal", () => {
  it("advances from root mapping to preview", async () => {
    const { props } = renderWizard({ step: "root_mapping" });
    expect(screen.getByText(/Map each source folder/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Preview baseline" }));
    expect(props.onPreview).toHaveBeenCalledTimes(1);
  });

  it("confirms the upload from the preview step", async () => {
    const { props } = renderWizard({ step: "preview" });
    expect(screen.getByText("Hokkaido")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /confirm.*upload baseline/i }));
    expect(props.onConfirm).toHaveBeenCalledTimes(1);
  });

  it("offers a safe retry on error and reports it", async () => {
    const { props } = renderWizard({ step: "error", error: "Server unreachable" });
    expect(screen.getByRole("alert")).toHaveTextContent("Server unreachable");
    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(props.onRetry).toHaveBeenCalledTimes(1);
  });

  it("finishes through onDone on completion", async () => {
    const { props } = renderWizard({ step: "complete" });
    await userEvent.click(screen.getByRole("button", { name: "Done" }));
    expect(props.onDone).toHaveBeenCalledTimes(1);
  });
});
