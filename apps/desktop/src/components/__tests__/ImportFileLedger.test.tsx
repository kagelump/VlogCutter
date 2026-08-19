import { render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import userEvent from "@testing-library/user-event";
import type { ImportCutoff, ImportPreflight, ImportPreviewItem, ImportRunReport, PreflightItem } from "../../types";
import { FileLedger } from "../ImportFileLedger";

vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) => `asset://localhost/${encodeURIComponent(path)}`,
}));

afterEach(() => {
  vi.unstubAllGlobals();
});

function item(overrides: Partial<PreflightItem> = {}): PreflightItem {
  return {
    path: "/Volumes/DJI_SD/DCIM/DJI_0925.MP4",
    relativePath: "DCIM/DJI_0925.MP4",
    filename: "DJI_0925.MP4",
    sizeBytes: 100,
    classification: "new",
    assetId: null,
    error: "",
    ...overrides,
  };
}

function cutoff(overrides: Partial<ImportCutoff> = {}): ImportCutoff {
  return {
    id: "cutoff_1",
    cardSessionId: "card_dji",
    cardLabel: "DJI_SD",
    relativePath: "DCIM/DJI_0925.MP4",
    filename: "DJI_0925.MP4",
    capturedAt: "2026-08-08T10:17:21Z",
    createdAt: "2026-08-20T09:00:00Z",
    updatedAt: "2026-08-20T09:00:00Z",
    ...overrides,
  };
}

function plan(items: PreflightItem[]): ImportPreflight {
  return {
    sourcePath: "/Volumes/DJI_SD",
    card: null,
    destination: {
      copies: false,
      archiveRoot: null,
      resolvedPath: "/Volumes/DJI_SD",
      summary: "Originals stay where they are",
      bytesToCopy: 0,
      availableBytes: null,
      ready: true,
      blockedReason: "",
    },
    scanned: items.length,
    newClips: items.length,
    knownClips: 0,
    relinkable: 0,
    missingProxies: 0,
    collisions: 0,
    unreadable: 0,
    items,
  };
}

function preview(overrides: Partial<ImportPreviewItem> = {}): ImportPreviewItem {
  return {
    path: "/Volumes/DJI_SD/DCIM/DJI_0925.MP4",
    thumbnailPath: "/cache/import-preview/fingerprint_dji_0925.jpg",
    durationSeconds: 65.8,
    capturedAt: "2026-08-08T10:17:21Z",
    error: null,
    ...overrides,
  };
}

describe("FileLedger row presentation", () => {
  it("shows a normal row: poster, capture time, duration, filename, and classification", () => {
    const normalItem = item();
    const previews = new Map([[normalItem.path, preview()]]);
    render(<FileLedger plan={plan([normalItem])} previews={previews} />);

    const row = screen.getByText("DJI_0925.MP4").closest("li")!;
    const image = row.querySelector("img")!;
    expect(image.getAttribute("src")).toBe(
      "asset://localhost/%2Fcache%2Fimport-preview%2Ffingerprint_dji_0925.jpg",
    );
    expect(row.querySelector(".import-file-thumb-placeholder")).toBeNull();
    expect(document.querySelector('time[datetime="2026-08-08T10:17:21Z"]')).toBeTruthy();
    expect(screen.getByText("1:05")).toBeTruthy();
    expect(screen.getByText("new")).toBeTruthy();
    expect(row.dataset.attention).toBeUndefined();
  });

  it("shows an explicit placeholder and unknown values when nothing has been probed yet", () => {
    const unknownItem = item({ path: "/Volumes/DJI_SD/DCIM/DJI_0926.MP4", filename: "DJI_0926.MP4" });
    render(<FileLedger plan={plan([unknownItem])} />);

    const row = screen.getByText("DJI_0926.MP4").closest("li")!;
    expect(row.querySelector("img")).toBeNull();
    // The extension glyph is the explicit placeholder — never a broken <img>.
    expect(row.querySelector(".import-file-thumb-placeholder")?.textContent).toBe("MP4");
    const time = row.querySelector("time")!;
    expect(time.textContent).toBe("—");
    expect(time.getAttribute("datetime")).toBeNull();
    expect(row.querySelector(".import-file-duration")?.textContent).toBe("—");
  });

  it("falls back to the cheap filesystem timestamp before anything is probed, and labels it as such", () => {
    const estimatedItem = item({
      path: "/Volumes/DJI_SD/DCIM/DJI_0927.MP4",
      filename: "DJI_0927.MP4",
      filesystemModifiedAt: "2026-08-08T09:00:00.000Z",
    });
    render(<FileLedger plan={plan([estimatedItem])} />);

    const time = document.querySelector('time[datetime="2026-08-08T09:00:00.000Z"]')!;
    expect(time).toBeTruthy();
    expect(time.getAttribute("title")).toMatch(/not a probed capture time/);
  });

  it("shows a warning row: the attention marker, its error, and the file still degrades rather than disappearing", () => {
    const warningItem = item({
      path: "/Volumes/DJI_SD/DCIM/C0002.MP4",
      filename: "C0002.MP4",
      classification: "unreadable",
      error: "Permission denied",
    });
    render(<FileLedger plan={plan([warningItem])} />);

    const row = screen.getByText("C0002.MP4").closest("li")!;
    expect(row.dataset.attention).toBe("true");
    expect(screen.getByText("Permission denied")).toBeTruthy();
    expect(screen.getByText("unreadable")).toBeTruthy();
    // A row that failed to classify still renders a complete, non-blocking row.
    expect(row.querySelector(".import-file-thumb-placeholder")).toBeTruthy();
  });

  it("prefers the completed import's real probed values over the preflight/preview estimates", () => {
    const importedItem = item();
    const previews = new Map([[importedItem.path, preview({ durationSeconds: 1, capturedAt: "2020-01-01T00:00:00Z" })]]);
    const report: ImportRunReport = {
      outcome: { imported: 1, importedAssetIds: ["asset_1"], relinked: 0, relinkedAssetIds: [], skipped: 0, copied: 1, bytesCopied: 100, cancelled: false, failures: [] },
      proxies: null,
      files: [{ path: importedItem.path, timestamp: "2026-08-08T10:17:21Z", durationSeconds: 65.8 }],
    };
    render(<FileLedger plan={plan([importedItem])} report={report} previews={previews} />);

    expect(document.querySelector('time[datetime="2026-08-08T10:17:21Z"]')).toBeTruthy();
    expect(screen.getByText("1:05")).toBeTruthy();
  });
});

describe("FileLedger lazy/bounded preview requests", () => {
  class ImmediateIntersectionObserver implements IntersectionObserver {
    readonly root = null;
    readonly rootMargin = "";
    readonly thresholds = [];
    constructor(private readonly callback: IntersectionObserverCallback) {}
    observe(target: Element) {
      this.callback([{ target, isIntersecting: true } as IntersectionObserverEntry], this);
    }
    unobserve() {}
    disconnect() {}
    takeRecords(): IntersectionObserverEntry[] {
      return [];
    }
  }

  it("asks only for rows that became visible and have no preview yet", () => {
    vi.stubGlobal("IntersectionObserver", ImmediateIntersectionObserver);
    const withPreview = item({ path: "/Volumes/DJI_SD/DCIM/DJI_0001.MP4", filename: "DJI_0001.MP4" });
    const withoutPreview = item({ path: "/Volumes/DJI_SD/DCIM/DJI_0002.MP4", filename: "DJI_0002.MP4" });
    const previews = new Map([[withPreview.path, preview({ path: withPreview.path })]]);
    const onVisible = vi.fn();

    render(<FileLedger plan={plan([withPreview, withoutPreview])} previews={previews} onVisible={onVisible} />);

    expect(onVisible).toHaveBeenCalledTimes(1);
    expect(onVisible).toHaveBeenCalledWith([withoutPreview.path]);
  });

  it("never requests previews when no IntersectionObserver is available (jsdom default), degrading to placeholders only", () => {
    expect(typeof IntersectionObserver).toBe("undefined");
    const onVisible = vi.fn();
    render(<FileLedger plan={plan([item()])} onVisible={onVisible} />);
    expect(onVisible).not.toHaveBeenCalled();
  });
});

/**
 * The durable "start here" boundary (#66). The invariant under test is that a
 * persistent filter is never invisible: the boundary names itself, says how
 * much it is hiding, and can be moved or cleared from the same list it filters.
 */
describe("FileLedger cutoff presentation", () => {
  const items = [
    item({ path: "/a/DCIM/DJI_0923.MP4", relativePath: "DCIM/DJI_0923.MP4", filename: "DJI_0923.MP4", classification: "excludedByCutoff" }),
    item({ path: "/a/DCIM/DJI_0924.MP4", relativePath: "DCIM/DJI_0924.MP4", filename: "DJI_0924.MP4", classification: "excludedByCutoff" }),
    item({ path: "/a/DCIM/DJI_0925.MP4", relativePath: "DCIM/DJI_0925.MP4", filename: "DJI_0925.MP4" }),
    item({ path: "/a/DCIM/DJI_0926.MP4", relativePath: "DCIM/DJI_0926.MP4", filename: "DJI_0926.MP4" }),
  ];

  function withCutoff(overrides: Partial<ImportPreflight> = {}): ImportPreflight {
    return { ...plan(items), cutoff: cutoff(), excludedByCutoff: 2, newClips: 2, ...overrides };
  }

  it("names the boundary and says how many earlier clips it is hiding", () => {
    render(<FileLedger plan={withCutoff()} onSetCutoff={vi.fn()} onClearCutoff={vi.fn()} />);

    const banner = screen.getByTestId("import-cutoff-banner");
    expect(within(banner).getByText("Importing from DJI_0925.MP4")).toBeVisible();
    expect(within(banner).getByText(/2 earlier clips are excluded\./)).toBeVisible();
    expect(within(banner).getByText(/Remembered for this card, DJI_SD, until you clear it\./)).toBeVisible();
  });

  it("marks the boundary row and dims what it excluded, without dropping any of it", () => {
    const { container } = render(<FileLedger plan={withCutoff()} onSetCutoff={vi.fn()} />);

    // Every candidate is still listed: a filter you cannot see through is not
    // inspectable, and moving the boundary earlier needs those rows.
    expect(container.querySelectorAll("li")).toHaveLength(4);
    expect(container.querySelector('li[data-cutoff="boundary"]')?.getAttribute("data-path")).toBe("/a/DCIM/DJI_0925.MP4");
    expect(container.querySelectorAll('li[data-cutoff="excluded"]')).toHaveLength(2);
    expect(screen.getByText("Import starts here")).toBeVisible();
    expect(screen.getAllByText("excluded by cutoff")).toHaveLength(2);
  });

  it("moves the boundary from any row, including one it currently excludes", async () => {
    const user = userEvent.setup();
    const onSetCutoff = vi.fn();
    render(<FileLedger plan={withCutoff()} onSetCutoff={onSetCutoff} />);

    await user.click(screen.getByRole("button", { name: /Import from DJI_0923.MP4 onward/ }));
    expect(onSetCutoff).toHaveBeenCalledWith(expect.objectContaining({ filename: "DJI_0923.MP4" }));

    // The boundary row offers no "from here": it already is here.
    expect(screen.queryByRole("button", { name: /Import from DJI_0925.MP4 onward/ })).toBeNull();
  });

  it("clears the boundary from the same list it filters", async () => {
    const user = userEvent.setup();
    const onClearCutoff = vi.fn();
    render(<FileLedger plan={withCutoff()} onSetCutoff={vi.fn()} onClearCutoff={onClearCutoff} />);

    await user.click(screen.getByRole("button", { name: "Clear cutoff" }));
    expect(onClearCutoff).toHaveBeenCalledTimes(1);
  });

  it("says out loud that a lost original before the boundary is still reconnected", () => {
    const withRelink = withCutoff({
      items: [
        item({ path: "/a/DCIM/DJI_0923.MP4", relativePath: "DCIM/DJI_0923.MP4", filename: "DJI_0923.MP4", classification: "knownRelinkable", assetId: "asset_lost" }),
        ...items.slice(1),
      ],
      excludedByCutoff: 1,
      relinkableBeforeCutoff: 1,
    });
    const { container } = render(<FileLedger plan={withRelink} onSetCutoff={vi.fn()} />);

    expect(container.querySelector('li[data-cutoff="relinkBeforeCutoff"]')).not.toBeNull();
    expect(screen.getByText("Before the cutoff · original still needed")).toBeVisible();
    expect(screen.getByText(/the cutoff never hides an original this project has lost/)).toBeVisible();
  });

  it("offers no boundary controls in the completed-run report: that is a record, not a decision", () => {
    const report: ImportRunReport = {
      outcome: { imported: 2, importedAssetIds: [], relinked: 0, relinkedAssetIds: [], skipped: 0, copied: 2, bytesCopied: 1, cancelled: false, failures: [] },
      proxies: null,
      files: [],
    };
    render(<FileLedger plan={withCutoff()} report={report} onSetCutoff={vi.fn()} onClearCutoff={vi.fn()} />);

    expect(screen.queryByTestId("import-cutoff-banner")).toBeNull();
    expect(screen.queryByRole("button", { name: /Import from/ })).toBeNull();
  });

  it("leaves a plan with no boundary exactly as it was", () => {
    const { container } = render(<FileLedger plan={plan(items.slice(2))} onSetCutoff={vi.fn()} />);
    expect(screen.queryByTestId("import-cutoff-banner")).toBeNull();
    expect(container.querySelectorAll("li[data-cutoff]")).toHaveLength(0);
  });
});
