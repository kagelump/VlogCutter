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

describe("ImportHistoryDialog", () => {
  it("requires a second explicit confirmation and explains preservation/modified clips", async () => {
    const onUndo = vi.fn();
    const user = userEvent.setup();
    const view = render(<ImportHistoryDialog sessions={[session]} detail={session} preview={null} onOpen={vi.fn()} onPreview={vi.fn()} onUndo={onUndo} onClose={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: /remove from project/i }));
    expect(onUndo).not.toHaveBeenCalled();
    view.rerender(<ImportHistoryDialog sessions={[session]} detail={session} preview={{ sessionId: "import_1", removableAssetCount: 2, alreadyRemovedAssetCount: 0, modifiedClips: [{ assetId: "asset_1", filename: "edited.MP4", reasons: ["notes"] }], affectedPhaseCount: 0, message: "Derived records are removed." }} onOpen={vi.fn()} onPreview={vi.fn()} onUndo={onUndo} onClose={vi.fn()} />);
    expect(screen.getByText(/Original files and archive copies will not be changed/i)).toBeTruthy();
    expect(screen.getByText(/edited.MP4/)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: /confirm remove from project/i }));
    expect(onUndo).toHaveBeenCalledWith("import_1");
  });
});
