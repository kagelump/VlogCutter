import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ImportHistoryDialog } from "../ImportHistoryDialog";

const session = {
  id: "import_1", projectId: "project_1", sourcePath: "/Volumes/CARD", destinationMode: "archive_copy" as const,
  destinationRootId: "root_1", destinationName: "Archive SSD", destinationPath: "/Archive/CARD",
  imported: 2, relinked: 1, skipped: 3, copied: 2, bytesCopied: 2, cancelled: false, failures: [],
  rolledBackAt: null, rolledBackAssetCount: 0, createdAt: "2026-08-10T12:00:00Z",
  files: [],
};

const preview = {
  sessionId: "import_1", removableAssetCount: 2, alreadyRemovedAssetCount: 0,
  modifiedClips: [{ assetId: "asset_1", filename: "edited.MP4", reasons: ["notes"] }],
  affectedPhaseCount: 0, message: "Annotations and derived records remain in the catalog.",
};

describe("ImportHistoryDialog", () => {
  it("requires a second explicit confirmation and truthfully explains preserved clip records", async () => {
    const onUndo = vi.fn();
    const user = userEvent.setup();
    const view = render(<ImportHistoryDialog sessions={[session]} detail={session} preview={null} error="" busy={false} onOpen={vi.fn()} onPreview={vi.fn()} onUndo={onUndo} onClose={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: /remove from project/i }));
    expect(onUndo).not.toHaveBeenCalled();
    view.rerender(<ImportHistoryDialog sessions={[session]} detail={session} preview={preview} error="" busy={false} onOpen={vi.fn()} onPreview={vi.fn()} onUndo={onUndo} onClose={vi.fn()} />);
    expect(screen.getByText(/annotations, and derived review records will be preserved/i)).toBeTruthy();
    expect(screen.getByText(/will be hidden from this project with their preserved annotations/i)).toBeTruthy();
    expect(screen.getByText(/edited.MP4/)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: /confirm remove from project/i }));
    expect(onUndo).toHaveBeenCalledWith("import_1");
  });

  it("shows a failure as an alert and keeps the confirmation armed", () => {
    render(<ImportHistoryDialog sessions={[session]} detail={session} preview={preview} error="catalog is locked" busy={false} onOpen={vi.fn()} onPreview={vi.fn()} onUndo={vi.fn()} onClose={vi.fn()} />);
    expect(screen.getByRole("alert").textContent).toBe("catalog is locked");
    // The removal the user asked for did not happen; the button that asks for
    // it again has to still be there.
    expect(screen.getByRole("button", { name: /confirm remove from project/i })).toBeTruthy();
  });

  it("refuses a second confirmation while one removal is in flight", () => {
    render(<ImportHistoryDialog sessions={[session]} detail={session} preview={preview} error="" busy onOpen={vi.fn()} onPreview={vi.fn()} onUndo={vi.fn()} onClose={vi.fn()} />);
    expect(screen.getByRole("button", { name: /confirm remove from project/i }).hasAttribute("disabled")).toBe(true);
  });
});
