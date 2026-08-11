import { render, screen, fireEvent, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SyncDiagnosticsModal } from "../SyncDiagnosticsModal";
import type { SyncStatus } from "../../types";

function status(overrides: Partial<SyncStatus> = {}): SyncStatus {
  return {
    projectId: "project_1",
    state: "synced",
    lastSyncAt: "2026-08-10T12:00:00Z",
    lastError: null,
    localPendingOperations: 0,
    remotePendingOperations: 0,
    conflictCount: 0,
    conflictDetails: [],
    activeEndpoint: "https://mac.local",
    backoffUntil: null,
    ...overrides,
  };
}

describe("SyncDiagnosticsModal", () => {
  it("triggers a sync and closes from its own controls", async () => {
    const onSyncNow = vi.fn();
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<SyncDiagnosticsModal status={status()} onSyncNow={onSyncNow} onClose={onClose} />);

    await user.click(screen.getByRole("button", { name: "Sync now" }));
    expect(onSyncNow).toHaveBeenCalledTimes(1);

    await user.click(screen.getByRole("button", { name: "Close" }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("closes on Escape from the backdrop", () => {
    const onClose = vi.fn();
    render(<SyncDiagnosticsModal status={status()} onSyncNow={vi.fn()} onClose={onClose} />);
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("surfaces unresolved conflicts with both sides", () => {
    render(
      <SyncDiagnosticsModal
        status={status({
          state: "attention",
          conflictCount: 1,
          conflictDetails: [
            {
              conflictId: "c1",
              conflictType: "clock_offset",
              aggregateType: "asset",
              aggregateId: "asset_1",
              expectedBaseRevision: 3,
              actualRevision: 4,
              createdAt: "2026-08-10T12:00:00Z",
              incomingOperationSummary: { operationId: "op_b", operationType: "set_offset", originNodeId: "node_b", originSequence: 2, camera: "Sony", startFilename: null, offsetSeconds: 12, label: null },
              activeOperationSummary: { operationId: "op_a", operationType: "set_offset", originNodeId: "node_a", originSequence: 1, camera: "Sony", startFilename: null, offsetSeconds: 0, label: null },
            },
          ],
        })}
        onSyncNow={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByText("1 unresolved")).toBeInTheDocument();
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByText(/Incoming/)).toBeInTheDocument();
    expect(within(dialog).getByText(/Active/)).toBeInTheDocument();
    expect(within(dialog).getByText(/Node: node_b/)).toBeInTheDocument();
  });
});
