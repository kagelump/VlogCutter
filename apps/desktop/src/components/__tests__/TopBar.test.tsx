import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { Project, SyncStatus } from "../../types";
import { TopBar, type OperationProgress, type TopBarProps } from "../TopBar";

function project(id: string, name: string): Project {
  return { id, name, assetCount: 0, labeledCount: 0 } as unknown as Project;
}

function progress(overrides: Partial<OperationProgress> = {}): OperationProgress {
  return {
    kind: "proxy",
    completed: 2,
    total: 10,
    completedWorkUnits: 0,
    totalWorkUnits: 0,
    workerCount: 2,
    estimatedRemainingSeconds: 45,
    ...overrides,
  };
}

function renderBar(overrides: Partial<TopBarProps> = {}) {
  const props: TopBarProps = {
    projects: [project("p1", "Kyoto autumn")],
    projectId: "p1",
    mode: "review",
    isDesktop: true,
    busy: false,
    canPairWithServer: false,
    syncStatus: null,
    operationProgress: null,
    error: "",
    message: "",
    missingProxyCount: 0,
    missingThumbnailCount: 0,
    missingAnalysisCount: 0,
    mediaMenuOpen: false,
    hasLastImport: false,
    interruptedImportLabel: "",
    labeledCount: 0,
    activeCount: 0,
    onProjectChange: vi.fn(),
    onModeChange: vi.fn(),
    onOpenCatalog: vi.fn(),
    onOpenSettings: vi.fn(),
    onOpenCreateProject: vi.fn(),
    onOpenShortcuts: vi.fn(),
    onPairWithServer: vi.fn(),
    onShowSyncDiagnostics: vi.fn(),
    onCancelAnalysis: vi.fn(),
    onCancelImport: vi.fn(),
    onDismissError: vi.fn(),
    onMakeProxies: vi.fn(),
    onToggleMediaMenu: vi.fn(),
    onMakeThumbnails: vi.fn(),
    onOpenAnalysis: vi.fn(),
    onImportFolder: vi.fn(),
    onReviewLastImport: vi.fn(),
    onResumeInterruptedImport: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<TopBar {...props} />) };
}

describe("the mode switcher", () => {
  it("shows all three modes rather than hiding them in a menu", () => {
    const nav = renderBar().container.querySelector(".mode-switcher")!;
    expect([...nav.querySelectorAll("button")].map((node) => node.textContent))
      .toEqual(["Review", "Chronology", "Edit plan"]);
  });

  it("marks the active mode as pressed", () => {
    renderBar({ mode: "chronology" });
    expect(screen.getByRole("button", { name: "Chronology" }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("button", { name: "Review" }).getAttribute("aria-pressed")).toBe("false");
  });

  it("keeps only Review reachable before a project is open", () => {
    renderBar({ projectId: "" });
    expect(screen.getByRole("button", { name: "Review" }).hasAttribute("disabled")).toBe(false);
    expect(screen.getByRole("button", { name: "Chronology" }).hasAttribute("disabled")).toBe(true);
    expect(screen.getByRole("button", { name: "Edit plan" }).hasAttribute("disabled")).toBe(true);
  });

  it("reports the mode that was chosen", async () => {
    const { props } = renderBar();
    await userEvent.click(screen.getByRole("button", { name: "Edit plan" }));
    expect(props.onModeChange).toHaveBeenCalledWith("editPlan");
  });
});

describe("the operation slot", () => {
  it("stays empty at idle — that emptiness is the point", () => {
    const slot = renderBar().getByTestId("operation-slot");
    expect(slot.textContent).toBe("");
  });

  it("names the running operation and its progress", () => {
    renderBar({ operationProgress: progress({ kind: "proxy" }) });
    const slot = screen.getByTestId("operation-slot");
    expect(within(slot).getByText("Generating proxies")).toBeTruthy();
    expect(within(slot).getByText("2/10")).toBeTruthy();
    expect(within(slot).getByText(/45s remaining · 2 workers/)).toBeTruthy();
  });

  it("names the analysis stage and what it has spent", () => {
    renderBar({
      operationProgress: progress({ kind: "analysis", stage: "transcripts", stageIndex: 1, stageCount: 3, costUsd: 0.42 }),
    });
    const slot = screen.getByTestId("operation-slot");
    expect(within(slot).getByText("Transcribing · step 2 of 3")).toBeTruthy();
    expect(within(slot).getByText(/\$0\.42 spent/)).toBeTruthy();
  });

  it("offers cancellation for analysis and imports", async () => {
    const { props } = renderBar({ operationProgress: progress({ kind: "analysis", stage: "analysis" }) });
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(props.onCancelAnalysis).toHaveBeenCalled();

    document.body.innerHTML = "";
    renderBar({ operationProgress: progress({ kind: "thumbnail" }) });
    expect(screen.queryByRole("button", { name: "Cancel" })).toBeNull();

    document.body.innerHTML = "";
    const importing = renderBar({ operationProgress: progress({ kind: "import", importStage: "copying" }) });
    expect(screen.getByText("Copying and verifying originals")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(importing.props.onCancelImport).toHaveBeenCalled();
  });

  it("measures the bar in work units when it has them, and in clips otherwise", () => {
    const withUnits = renderBar({
      operationProgress: progress({ completed: 1, total: 10, completedWorkUnits: 30, totalWorkUnits: 60 }),
    });
    expect(withUnits.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("30");
    expect(withUnits.getByRole("progressbar").getAttribute("aria-valuemax")).toBe("60");

    document.body.innerHTML = "";
    const withoutUnits = renderBar({ operationProgress: progress({ completed: 4, total: 10 }) });
    expect(withoutUnits.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("4");
    expect(withoutUnits.getByRole("progressbar").getAttribute("aria-valuemax")).toBe("10");
  });

  it("gives an error the alert role and a way out", async () => {
    const { props } = renderBar({ error: "Catalog is locked" });
    expect(screen.getByRole("alert").textContent).toContain("Catalog is locked");
    await userEvent.click(screen.getByRole("button", { name: "Dismiss error" }));
    expect(props.onDismissError).toHaveBeenCalled();
  });

  it("lets progress win over an error, and an error over a message", () => {
    renderBar({ operationProgress: progress(), error: "Catalog is locked", message: "Saved" });
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByText("Saved")).toBeNull();

    document.body.innerHTML = "";
    renderBar({ error: "Catalog is locked", message: "Saved" });
    expect(screen.getByRole("alert")).toBeTruthy();
    expect(screen.queryByText("Saved")).toBeNull();
  });
});

describe("media generation", () => {
  it("hides the proxy button when there is no work, rather than disabling it", () => {
    renderBar({ missingProxyCount: 0 });
    expect(screen.queryByRole("button", { name: /Make .* proxies/ })).toBeNull();
  });

  it("shows the outstanding count once there is work", async () => {
    const { props } = renderBar({ missingProxyCount: 12 });
    await userEvent.click(screen.getByRole("button", { name: "Make 12 proxies" }));
    expect(props.onMakeProxies).toHaveBeenCalled();
  });

  it("keeps the menu items disabled but present when their work is done", () => {
    renderBar({ mediaMenuOpen: true, missingThumbnailCount: 0, missingAnalysisCount: 0 });
    expect(screen.getByText("Thumbnails ready").closest("button")!.hasAttribute("disabled")).toBe(true);
    expect(screen.getByText("Analysis up to date").closest("button")!.hasAttribute("disabled")).toBe(false);
  });

  it("offers no way back into an import when none was interrupted", () => {
    renderBar({ mediaMenuOpen: true, hasLastImport: true });
    expect(screen.queryByText("Resume interrupted import")).toBeNull();
  });

  it("leads with an interrupted import, above the history entry (#67)", async () => {
    const { props } = renderBar({
      mediaMenuOpen: true,
      hasLastImport: true,
      interruptedImportLabel: "Sony — Volume Untitled · waiting to reconnect",
    });

    const entry = screen.getByText("Resume interrupted import").closest("button")!;
    expect(within(entry).getByText("Sony — Volume Untitled · waiting to reconnect")).toBeTruthy();
    // Live work above history: "Review last import" is not a way back in.
    const history = screen.getByText("Review last import").closest("button")!;
    expect(entry.compareDocumentPosition(history) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    await userEvent.click(entry);
    expect(props.onResumeInterruptedImport).toHaveBeenCalledTimes(1);
  });

  it("hints ⇧G for thumbnails, since T belongs to the ribbon", () => {
    renderBar({ mediaMenuOpen: true, missingThumbnailCount: 4 });
    const item = screen.getByText("Generate 4 thumbnails").closest("button")!;
    expect(within(item).getByText("⇧G")).toBeTruthy();
    expect(item.getAttribute("aria-keyshortcuts")).toBe("Shift+G");
  });
});

describe("desktop-only affordances", () => {
  it("drops the catalog and pairing buttons in the browser build", () => {
    renderBar({ isDesktop: false, canPairWithServer: true });
    expect(screen.queryByRole("button", { name: "Open catalog" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Pair with server" })).toBeNull();
  });

  it("offers pairing on desktop once the server says it can be paired", () => {
    renderBar({ isDesktop: true, canPairWithServer: true });
    expect(screen.getByRole("button", { name: "Pair with server" })).toBeTruthy();
  });
});

describe("the sync indicator", () => {
  it("is absent while unpaired", () => {
    renderBar({ syncStatus: { state: "unpaired", lastError: null } as SyncStatus });
    expect(document.querySelector(".sync-indicator")).toBeNull();
  });

  it("names the state and carries the last error in its tooltip", async () => {
    const { props } = renderBar({
      syncStatus: { state: "attention", lastError: "clock skew" } as SyncStatus,
    });
    const button = document.querySelector<HTMLButtonElement>(".sync-indicator")!;
    expect(button.textContent).toContain("Needs attention");
    expect(button.title).toBe("Sync: attention — clock skew");
    await userEvent.click(button);
    expect(props.onShowSyncDiagnostics).toHaveBeenCalled();
  });
});

describe("the completion hairline", () => {
  it("reports labeled share of the active queue and survives an empty one", () => {
    const filled = renderBar({ labeledCount: 3, activeCount: 12 }).container
      .querySelector<HTMLElement>(".completion-hairline span")!;
    expect(filled.style.width).toBe("25%");

    document.body.innerHTML = "";
    const empty = renderBar({ labeledCount: 0, activeCount: 0 }).container
      .querySelector<HTMLElement>(".completion-hairline span")!;
    expect(empty.style.width).toBe("0%");
  });
});
