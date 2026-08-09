import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { ArchiveRoot, CardCandidate, ImportPreflight } from "../../types";
import { ImportDialog, type ImportDialogProps } from "../ImportDialog";

const card: CardCandidate = {
  sessionId: "card_4f2a0000",
  label: "DJI_SD",
  mountPath: "/Volumes/DJI_SD",
  cameraHint: "DJI Pocket 3",
  clipCount: 243,
  totalBytes: 80_000_000_000,
};

const root: ArchiveRoot = {
  id: "archive_nas",
  displayName: "NAS Video",
  localMountPath: "/Volumes/nas/VideoArchive",
  available: true,
  availableBytes: 1_200_000_000_000,
  createdAt: "2026-08-09T00:00:00Z",
};

function plan(overrides: Partial<ImportPreflight> = {}): ImportPreflight {
  return {
    sourcePath: card.mountPath,
    card,
    destination: {
      copies: true,
      archiveRoot: root,
      resolvedPath: "/Volumes/nas/VideoArchive/spain-2026/DJI_SD-4f2a0000",
      summary: "Originals are copied to NAS Video, and stay on the card too",
      bytesToCopy: 62_400_000_000,
      availableBytes: root.availableBytes,
      ready: true,
      blockedReason: "",
    },
    scanned: 243,
    newClips: 18,
    knownClips: 212,
    relinkable: 0,
    missingProxies: 31,
    collisions: 0,
    unreadable: 0,
    items: [],
    ...overrides,
  };
}

function renderDialog(overrides: Partial<ImportDialogProps> = {}) {
  const props: ImportDialogProps = {
    projectName: "Spain 2026",
    sourcePath: card.mountPath,
    card,
    roots: [root],
    selectedRootId: root.id,
    plan: plan(),
    loading: false,
    running: false,
    progress: null,
    report: null,
    error: "",
    verifyCopies: true,
    makeProxies: true,
    onSelectRoot: vi.fn(),
    onCreateRoot: vi.fn(),
    onLocateRoot: vi.fn(),
    onVerifyCopies: vi.fn(),
    onMakeProxies: vi.fn(),
    onImport: vi.fn(),
    onCancel: vi.fn(),
    onClose: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<ImportDialog {...props} />) };
}

describe("card import preflight", () => {
  it("puts destination, copy cost, free space, and delta before the action", () => {
    renderDialog();
    expect(screen.getAllByText("NAS Video")).toHaveLength(2);
    expect(screen.getByText(/62.4 GB/)).toBeTruthy();
    expect(screen.getByText(/1.2 TB free/)).toBeTruthy();
    expect(screen.getByText("new to Spain 2026").previousElementSibling?.textContent).toBe("18");
    expect(screen.getByRole("button", { name: "Import 18 clips" })).toBeTruthy();
  });

  it("blocks import when the destination cannot hold the copy", () => {
    const blocked = plan({
      destination: {
        ...plan().destination,
        ready: false,
        blockedReason: "NAS Video has 10.0 GB free but this import needs 62.4 GB",
      },
    });
    renderDialog({ plan: blocked });
    expect(screen.getByRole("alert").textContent).toContain("10.0 GB free");
    expect(screen.getByRole("button", { name: "Import 18 clips" }).hasAttribute("disabled")).toBe(true);
  });

  it("offers inline first-run archive setup", async () => {
    const { props } = renderDialog({ roots: [], selectedRootId: "", plan: null });
    await userEvent.type(screen.getByLabelText("Name this archive"), "Shuttle SSD");
    await userEvent.click(screen.getByRole("button", { name: "Choose folder…" }));
    expect(props.onCreateRoot).toHaveBeenCalledWith("Shuttle SSD");
  });

  it("states plainly when a normal folder remains referenced in place", () => {
    const folderPlan = plan({
      card: null,
      destination: {
        copies: false,
        archiveRoot: null,
        resolvedPath: "/Volumes/NAS/Spain",
        summary: "Originals stay where they are, in /Volumes/NAS/Spain",
        bytesToCopy: 0,
        availableBytes: null,
        ready: true,
        blockedReason: "",
      },
    });
    renderDialog({ card: null, sourcePath: "/Volumes/NAS/Spain", roots: [], selectedRootId: "", plan: folderPlan });
    expect(screen.getByText("Stay in place")).toBeTruthy();
    expect(screen.getByText("No originals are copied")).toBeTruthy();
  });
});
