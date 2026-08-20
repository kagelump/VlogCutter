/**
 * The decisions behind the paused import surface (#79), with nothing rendered.
 * Whether to continue on our own, keep waiting, or ask — and what to call the
 * volume meanwhile — are all answerable without a shell.
 */

import { describe, expect, it } from "vitest";
import {
  pauseFromReport,
  pausedDetail,
  pausedHeadline,
  resumeDecision,
  type PausedImport,
  type PausedImportOptions,
} from "./pausedImport";
import type { ImportRunReport } from "../types";

const options: PausedImportOptions = {
  projectId: "project_eclipse",
  sourceLabel: "SD_Card",
  destinationLabel: "Acasis",
};

function report(outcome: Partial<ImportRunReport["outcome"]> = {}): ImportRunReport {
  return {
    outcome: {
      imported: 77,
      importedAssetIds: [],
      relinked: 0,
      relinkedAssetIds: [],
      skipped: 0,
      copied: 77,
      bytesCopied: 0,
      cancelled: false,
      failures: [],
      ...outcome,
    },
  } as ImportRunReport;
}

function paused(overrides: Partial<PausedImport> = {}): PausedImport {
  return {
    endpoint: "destination",
    volumeLabel: "Acasis",
    reachability: "not-connected",
    committed: 77,
    failed: 0,
    pausedAt: 1_000,
    ...overrides,
  };
}

describe("pauseFromReport", () => {
  it("pauses on the destination when the destination went away", () => {
    const state = pauseFromReport(options, report({ destinationDisconnected: true }), 1_000);

    expect(state).not.toBeNull();
    expect(state?.endpoint).toBe("destination");
    expect(state?.volumeLabel).toBe("Acasis");
    expect(state?.committed).toBe(77);
  });

  it("pauses on the source when the card went away", () => {
    const state = pauseFromReport(options, report({ sourceDisconnected: true }), 1_000);

    expect(state?.endpoint).toBe("source");
    expect(state?.volumeLabel).toBe("SD_Card");
  });

  /// A stalled volume is still mounted. Saying it "isn't connected" while its
  /// icon sits in Finder is the kind of wrongness a user notices.
  it("distinguishes a volume that stalled from one that vanished", () => {
    const stalled = pauseFromReport(
      options,
      report({
        destinationDisconnected: true,
        failures: [
          { path: "/Volumes/Acasis/clip.MP4", error: "fingerprinting stopped answering after 60s" },
        ],
      }),
      1_000,
    );
    const vanished = pauseFromReport(options, report({ destinationDisconnected: true }), 1_000);

    expect(stalled?.reachability).toBe("not-responding");
    expect(vanished?.reachability).toBe("not-connected");
  });

  it("is not a pause when the user cancelled", () => {
    expect(pauseFromReport(options, report({ cancelled: true }), 1_000)).toBeNull();
  });

  it("is not a pause when the run simply finished", () => {
    expect(pauseFromReport(options, report(), 1_000)).toBeNull();
  });

  it("carries genuine per-file failures through the pause", () => {
    const state = pauseFromReport(
      options,
      report({
        sourceDisconnected: true,
        failures: [{ path: "/Volumes/SD_Card/bad.MP4", error: "unreadable" }],
      }),
      1_000,
    );

    expect(state?.failed).toBe(1);
  });
});

describe("resumeDecision", () => {
  it("continues on its own once the same volume is back", () => {
    expect(resumeDecision(paused(), { present: true, identityVerified: true })).toEqual({
      action: "continue",
    });
  });

  it("keeps waiting while nothing is there", () => {
    expect(resumeDecision(paused(), { present: false, identityVerified: false })).toEqual({
      action: "keep-waiting",
    });
  });

  /// The case that would otherwise write onto the wrong disk: something is
  /// there, and it cannot prove it is ours.
  it("asks rather than guessing when what returned cannot prove itself", () => {
    expect(resumeDecision(paused(), { present: true, identityVerified: false })).toEqual({
      action: "escalate",
      corrective: "locate-archive",
    });
  });

  it("escalates a source to choosing a source, not locating an archive", () => {
    expect(
      resumeDecision(paused({ endpoint: "source" }), { present: true, identityVerified: false }),
    ).toEqual({ action: "escalate", corrective: "choose-source" });
  });

  /// Identity is never inferred from presence, whichever end it is.
  it("never continues on presence alone", () => {
    for (const endpoint of ["source", "destination"] as const) {
      expect(
        resumeDecision(paused({ endpoint }), { present: true, identityVerified: false }).action,
      ).not.toBe("continue");
    }
  });
});

describe("paused copy", () => {
  it("keeps the headline constant so the detail line carries the news", () => {
    expect(pausedHeadline()).toBe("Import paused");
  });

  it("says a stalled volume is not responding, not that it is missing", () => {
    expect(pausedDetail(paused({ reachability: "not-responding" }))).toBe(
      "Acasis isn't responding",
    );
  });

  it("says a vanished volume is not connected", () => {
    expect(pausedDetail(paused({ reachability: "not-connected" }))).toBe(
      "Acasis isn't connected",
    );
  });
});
