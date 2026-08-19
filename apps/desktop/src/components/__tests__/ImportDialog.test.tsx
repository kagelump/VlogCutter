import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { ArchiveRoot, CardCandidate, ImportPreflight, ImportRunReport, PreflightItem } from "../../types";
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
    expect(screen.getByText("NAS Video")).toBeTruthy();
    expect(screen.getByText(/62.4 GB/)).toBeTruthy();
    expect(screen.getByText(/1.2 TB free/)).toBeTruthy();
    expect(screen.getByText("Ready to import")).toBeVisible();
    expect(screen.getByText("18 new clips · 31 proxies will be made")).toBeVisible();
    expect(screen.getByRole("button", { name: "Import 18 clips" })).toBeTruthy();
    expect(screen.getByRole("option", { name: `NAS Video — ${root.localMountPath}` })).toBeTruthy();
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
    expect(screen.getByText("Needs attention")).toBeVisible();
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

  it("shows filename, capture timestamp, and duration in the completed file ledger", async () => {
    const item: PreflightItem = {
      path: `${card.mountPath}/DCIM/DJI_001.MP4`,
      relativePath: "DCIM/DJI_001.MP4",
      filename: "DJI_001.MP4",
      sizeBytes: 100,
      classification: "new",
      assetId: null,
      error: "",
    };
    const report: ImportRunReport = {
      outcome: { imported: 1, importedAssetIds: ["asset_new"], relinked: 0, relinkedAssetIds: [], skipped: 0, copied: 1, bytesCopied: 100, cancelled: false, failures: [] },
      proxies: null,
      files: [{ path: item.path, timestamp: "2026-08-09T08:14:15Z", durationSeconds: 65.8 }],
    };
    renderDialog({ plan: plan({ scanned: 1, newClips: 1, items: [item] }), report });

    await userEvent.click(screen.getByText("Review every file"));
    expect(screen.getByText("DJI_001.MP4")).toBeTruthy();
    expect(screen.getByText("1:05")).toBeTruthy();
    expect(document.querySelector('time[datetime="2026-08-09T08:14:15Z"]')).toBeTruthy();
  });
});

describe("the all-clear / needs-attention decision", () => {
  it("leads with an unmistakable ready state when nothing needs a look", () => {
    renderDialog({
      plan: plan({ newClips: 8, knownClips: 0, relinkable: 0, missingProxies: 8, collisions: 0, unreadable: 0 }),
    });
    expect(screen.getByText("Ready to import")).toBeVisible();
    expect(screen.getByText("8 new clips · 8 proxies will be made")).toBeVisible();
    // No attention warning, and no chronology prose has to be read first.
    expect(screen.queryByText(/Review \d+ files?/)).toBeNull();
    expect(screen.getByRole("button", { name: "Import 8 clips" })).toBeEnabled();
  });

  it("flags collisions and unreadable files with a direct action to their rows", async () => {
    const collisionItem: PreflightItem = {
      path: `${card.mountPath}/DCIM/C0001.MP4`,
      relativePath: "DCIM/C0001.MP4",
      filename: "C0001.MP4",
      sizeBytes: 100,
      classification: "pathCollision",
      assetId: null,
      error: "Another file already claims this destination path",
    };
    const unreadableItem: PreflightItem = {
      path: `${card.mountPath}/DCIM/C0002.MP4`,
      relativePath: "DCIM/C0002.MP4",
      filename: "C0002.MP4",
      sizeBytes: 0,
      classification: "unreadable",
      assetId: null,
      error: "Permission denied",
    };
    const cleanItem: PreflightItem = {
      path: `${card.mountPath}/DCIM/C0003.MP4`,
      relativePath: "DCIM/C0003.MP4",
      filename: "C0003.MP4",
      sizeBytes: 100,
      classification: "new",
      assetId: null,
      error: "",
    };
    renderDialog({
      plan: plan({
        newClips: 1,
        knownClips: 0,
        relinkable: 0,
        missingProxies: 1,
        collisions: 1,
        unreadable: 1,
        items: [collisionItem, unreadableItem, cleanItem],
      }),
    });

    expect(screen.getByText("Needs attention")).toBeVisible();
    expect(screen.getByText(/1 path collision · 1 unreadable file/)).toBeVisible();
    // Import is not a dead end: it never blocks on collisions/unreadable
    // files alone, since those are simply excluded from the copy.
    expect(screen.getByRole("button", { name: "Import 1 clip" })).toBeEnabled();

    const reviewButton = screen.getByRole("button", { name: "Review 2 files" });
    // The ledger starts collapsed — nothing is visible before the action.
    expect(screen.queryByText("C0001.MP4")).not.toBeVisible();

    await userEvent.click(reviewButton);

    const collisionRow = screen.getByText("C0001.MP4").closest("li")!;
    expect(collisionRow).toBeVisible();
    expect(collisionRow).toHaveFocus();
  });

  it("shows nothing new to import, and a note about clips still missing proxies", () => {
    renderDialog({
      plan: plan({ newClips: 0, knownClips: 0, relinkable: 0, missingProxies: 6, collisions: 0, unreadable: 0 }),
    });
    expect(screen.getByText("Nothing new to import")).toBeVisible();
    expect(screen.getByText("6 clips already imported still need proxies")).toBeVisible();
    expect(screen.getByRole("button", { name: "Nothing new" })).toBeDisabled();
  });
});

describe("camera-first title", () => {
  it("leads with the camera, not a generic volume name, on its own clip-count line", () => {
    renderDialog({
      card: { ...card, label: "Untitled", cameraHint: "Sony" },
      plan: plan({ card: { ...card, label: "Untitled", cameraHint: "Sony" } }),
    });
    expect(screen.getByRole("heading", { name: "Sony — Volume Untitled" })).toBeVisible();
    expect(screen.getByText("243 clips")).toBeVisible();
  });

  it("falls back to the volume label when no camera family is recognized", () => {
    const genericCard = { ...card, label: "NO NAME", cameraHint: "" };
    renderDialog({ card: genericCard, plan: plan({ card: genericCard }) });
    expect(screen.getByRole("heading", { name: "NO NAME" })).toBeVisible();
  });
});

describe("summary reading across new/known/mixed/relink/proxy combinations", () => {
  it("reads correctly for an all-new card", () => {
    renderDialog({ plan: plan({ newClips: 8, knownClips: 0, relinkable: 0, missingProxies: 8 }) });
    expect(screen.getByText("Ready to import")).toBeVisible();
    expect(screen.getByText("8 new clips · 8 proxies will be made")).toBeVisible();
  });

  it("reads correctly for an all-known card with nothing to do", () => {
    renderDialog({ plan: plan({ newClips: 0, knownClips: 8, relinkable: 0, missingProxies: 0 }) });
    expect(screen.getByText("Nothing new to import")).toBeVisible();
    expect(screen.queryByText(/proxies will be made|still need proxies/)).toBeNull();
  });

  it("reads correctly for a mixed card of new and already-known clips", () => {
    renderDialog({ plan: plan({ newClips: 5, knownClips: 3, relinkable: 0, missingProxies: 5 }) });
    expect(screen.getByText("Ready to import")).toBeVisible();
    expect(screen.getByText("5 new clips · 5 proxies will be made")).toBeVisible();
  });

  it("reads correctly for a relink-only card whose originals moved", () => {
    renderDialog({ plan: plan({ newClips: 0, knownClips: 0, relinkable: 4, missingProxies: 0 }) });
    expect(screen.getByText("Ready to import")).toBeVisible();
    expect(screen.getByText("4 clips to reconnect")).toBeVisible();
    expect(screen.getByRole("button", { name: "Reconnect 4 clips" })).toBeEnabled();
  });

  it("reads correctly for a card that only needs missing proxies made", () => {
    renderDialog({ plan: plan({ newClips: 0, knownClips: 0, relinkable: 0, missingProxies: 6 }) });
    expect(screen.getByText("Nothing new to import")).toBeVisible();
    expect(screen.getByText("6 clips already imported still need proxies")).toBeVisible();
  });
});

describe("chronology preflight, collapsed by default", () => {
  it("collapses a routine sequence proposal to one line ready for later review", () => {
    renderDialog({
      plan: plan({
        sequencePreview: {
          newClipCount: 7,
          existingSequenceCount: 0,
          proposedSequenceCount: 1,
          matchedExistingSequenceCount: 0,
          suggestedNewSequenceCount: 1,
          boundaries: [],
          runs: [{
            firstRelativePath: "DCIM/100MSDCF/C0001.MP4",
            lastRelativePath: "DCIM/100MSDCF/C0007.MP4",
            clipCount: 7,
            cameraKey: "Sony A7C II",
            volumeHint: "card_new",
            filenamePrefix: "C",
            decision: "proposeNew",
            matchedExistingSequenceId: null,
            matchedExistingSequenceLabel: null,
            comparedExistingSequenceId: null,
            comparedExistingSequenceLabel: null,
            needsReview: false,
            reason: "First footage seen from this camera.",
            evidence: [],
          }],
        },
      }),
    });

    const summary = screen.getByText("Chronology suggestions ready for later review");
    expect(summary).toBeVisible();
    // Routine detail stays behind the disclosure; it is present, but not the
    // headline of the dialog, and not something the user must read first.
    expect(screen.getByText("Proposes a new sequence for 7 clips")).not.toBeVisible();
  });

  it("keeps a genuine chronology conflict elevated and visually distinct from a routine one", () => {
    renderDialog({
      plan: plan({
        sequencePreview: {
          newClipCount: 18,
          existingSequenceCount: 1,
          proposedSequenceCount: 2,
          matchedExistingSequenceCount: 1,
          suggestedNewSequenceCount: 1,
          boundaries: [{
            beforeRelativePath: "DCIM/100MSDCF/C0241.MP4",
            afterRelativePath: "DCIM/101MSDCF/C0001.MP4",
            affectedClipCount: 7,
            evidence: [
              { kind: "folderReset", from: "DCIM/100MSDCF", to: "DCIM/101MSDCF" },
              { kind: "counterReset", previous: 241, next: 1 },
            ],
          }],
          runs: [{
            firstRelativePath: "DCIM/100MSDCF/C0231.MP4",
            lastRelativePath: "DCIM/100MSDCF/C0241.MP4",
            clipCount: 11,
            cameraKey: "Sony A7C II",
            volumeHint: "card_4f2a0000",
            filenamePrefix: "C",
            decision: "extendExisting",
            matchedExistingSequenceId: "sequence_sony_a",
            matchedExistingSequenceLabel: "Sony SD-A",
            comparedExistingSequenceId: null,
            comparedExistingSequenceLabel: null,
            needsReview: false,
            reason: "This run matches the camera, card, and filename evidence for “Sony SD-A”.",
            evidence: [],
          }, {
            firstRelativePath: "DCIM/101MSDCF/C0001.MP4",
            lastRelativePath: "DCIM/101MSDCF/C0007.MP4",
            clipCount: 7,
            cameraKey: "Sony A7C II",
            volumeHint: "card_new",
            filenamePrefix: "C",
            decision: "proposeNew",
            matchedExistingSequenceId: null,
            matchedExistingSequenceLabel: null,
            comparedExistingSequenceId: "sequence_sony_a",
            comparedExistingSequenceLabel: "Sony SD-A",
            needsReview: true,
            reason: "This run differs from “Sony SD-A” on card or filename evidence.",
            evidence: [
              { kind: "folderReset", from: "DCIM/100MSDCF", to: "DCIM/101MSDCF" },
              { kind: "counterReset", previous: 241, next: 1 },
              { kind: "volumeChange", from: "card_old", to: "card_new" },
            ],
          }],
        },
      }),
    });

    expect(screen.getByLabelText("Capture sequence preview").textContent).toContain("2 incoming card runs");
    // Both runs are directly visible — nothing is hidden behind a routine
    // collapse once one of them genuinely needs a look.
    const extendRow = screen.getByText("Extends “Sony SD-A” by 11 clips").closest("li")!;
    const attentionRow = screen.getByText("Proposes a new sequence for 7 clips").closest("li")!;
    expect(extendRow).toBeVisible();
    expect(attentionRow).toBeVisible();
    expect(extendRow.dataset.attention).toBeUndefined();
    expect(attentionRow.dataset.attention).toBe("true");
    expect(screen.getByText("Needs a look")).toBeVisible();
    expect(screen.getByText(/C0001.MP4 → C0007.MP4/)).toBeTruthy();
    expect(screen.getByText("Folder").getAttribute("title")).toContain("100MSDCF");
    expect(screen.getByText("Counter").getAttribute("title")).toContain("241 → 1");
    expect(screen.getByText("Volume").getAttribute("title")).toContain("card_old → card_new");
    expect(screen.getByText(/importing never changes chronology/)).toBeTruthy();
  });

  // A `proposeNew` run can name a `comparedExistingSequenceId` for entirely
  // routine reasons — core's own `needsReview` flag, not the mere presence
  // of that id, is what separates "genuinely disagrees" from "routine, but
  // happened to be compared against something." These two pin the routine
  // reason branches core can emit alongside a non-null compared id.
  it("stays routine when a run only lost its slot to another incoming run", () => {
    renderDialog({
      plan: plan({
        sequencePreview: {
          newClipCount: 4,
          existingSequenceCount: 1,
          proposedSequenceCount: 2,
          matchedExistingSequenceCount: 1,
          suggestedNewSequenceCount: 1,
          boundaries: [],
          runs: [{
            firstRelativePath: "DCIM/100MSDCF/C0001.MP4",
            lastRelativePath: "DCIM/100MSDCF/C0002.MP4",
            clipCount: 2,
            cameraKey: "Sony (C-series)",
            volumeHint: "card_new",
            filenamePrefix: "C",
            decision: "extendExisting",
            matchedExistingSequenceId: "sequence_sony_a",
            matchedExistingSequenceLabel: "Sony SD-A",
            comparedExistingSequenceId: null,
            comparedExistingSequenceLabel: null,
            needsReview: false,
            reason: "This run matches the camera, card, and filename evidence for “Sony SD-A”.",
            evidence: [],
          }, {
            firstRelativePath: "DCIM/101MSDCF/C0001.MP4",
            lastRelativePath: "DCIM/101MSDCF/C0002.MP4",
            clipCount: 2,
            cameraKey: "Sony (C-series)",
            volumeHint: "card_new",
            filenamePrefix: "C",
            decision: "proposeNew",
            matchedExistingSequenceId: null,
            matchedExistingSequenceLabel: null,
            comparedExistingSequenceId: "sequence_sony_a",
            comparedExistingSequenceLabel: "Sony SD-A",
            needsReview: false,
            reason: "Another incoming run already extends “Sony SD-A”; this distinct run stays separate.",
            evidence: [],
          }],
        },
      }),
    });

    // Nothing here needs a decision: the whole section collapses, and
    // neither run carries the attention marker.
    expect(screen.getByText("Chronology suggestions ready for later review")).toBeVisible();
    expect(screen.queryByText("Needs a look")).toBeNull();
    const displacedRow = screen.getByText(/already extends/).closest("li")!;
    expect(displacedRow.dataset.attention).toBeUndefined();
  });

  it("stays routine when boundary evidence, not a disagreement, kept a run separate", () => {
    renderDialog({
      plan: plan({
        sequencePreview: {
          newClipCount: 2,
          existingSequenceCount: 1,
          proposedSequenceCount: 1,
          matchedExistingSequenceCount: 0,
          suggestedNewSequenceCount: 1,
          boundaries: [],
          runs: [{
            firstRelativePath: "DCIM/101MSDCF/C0001.MP4",
            lastRelativePath: "DCIM/101MSDCF/C0002.MP4",
            clipCount: 2,
            cameraKey: "Sony (C-series)",
            volumeHint: "card_new",
            filenamePrefix: "C",
            decision: "proposeNew",
            matchedExistingSequenceId: null,
            matchedExistingSequenceLabel: null,
            comparedExistingSequenceId: "sequence_sony_a",
            comparedExistingSequenceLabel: "Sony SD-A",
            needsReview: false,
            reason: "Boundary evidence keeps this run separate from “Sony SD-A”.",
            evidence: [
              { kind: "folderReset", from: "DCIM/100MSDCF", to: "DCIM/101MSDCF" },
            ],
          }],
        },
      }),
    });

    expect(screen.getByText("Chronology suggestions ready for later review")).toBeVisible();
    expect(screen.queryByText("Needs a look")).toBeNull();
    const separatedRow = screen.getByText(/Boundary evidence keeps this run separate/).closest("li")!;
    expect(separatedRow.dataset.attention).toBeUndefined();
  });
});
