/**
 * Recovery decisions for an import whose source vanished (#67), with nothing
 * rendered. The questions that matter here — is this the same card, does a
 * dismissed banner block recovery, what survives the gap — are all answerable
 * without a shell.
 */

import { describe, expect, it } from "vitest";
import {
  detectsSourceAutomatically,
  interruptedProgressNote,
  interruptedSourceHeadline,
  interruptedFailureNote,
  interruptedMenuNote,
  interruptedSourceLabel,
  interruptionFromReport,
  isSameInterruptedSession,
  matchesInterruptedSource,
  selectMountedCards,
  type InterruptedImport,
} from "./interruptedImport";
import type { CardCandidate, ImportRunReport } from "../types";

function card(overrides: Partial<CardCandidate> = {}): CardCandidate {
  return {
    sessionId: "session_sony",
    label: "Untitled",
    mountPath: "/Volumes/Untitled",
    cameraHint: "Sony",
    clipCount: 243,
    totalBytes: 500_000,
    ...overrides,
  };
}

function report(overrides: Partial<ImportRunReport["outcome"]> = {}): ImportRunReport {
  return {
    outcome: {
      imported: 0,
      importedAssetIds: [],
      relinked: 0,
      relinkedAssetIds: [],
      skipped: 0,
      copied: 0,
      bytesCopied: 0,
      cancelled: false,
      sourceDisconnected: false,
      failures: [],
      ...overrides,
    },
    proxies: null,
    files: [],
  };
}

const options = {
  projectId: "project_1",
  sourcePath: "/Volumes/Untitled",
  card: card(),
  archiveRootId: "root_nas",
  cameraOverride: "Sony A7C II",
  verifyCopies: true,
  makeProxies: false,
};

function interrupted(overrides: Partial<InterruptedImport> = {}): InterruptedImport {
  return { ...options, committed: 12, failed: 0, interruptedAt: 1_700_000_000_000, ...overrides };
}

describe("interruptionFromReport", () => {
  it("records the session and the user's choices when the source disconnected", () => {
    const found = interruptionFromReport(
      options,
      report({ sourceDisconnected: true, imported: 10, relinked: 2 }),
      1_700_000_000_000,
    );
    expect(found).toEqual({
      ...options,
      committed: 12,
      failed: 0,
      interruptedAt: 1_700_000_000_000,
    });
  });

  it("leaves a cancelled run alone: stopping was the user's own decision", () => {
    expect(interruptionFromReport(options, report({ cancelled: true, imported: 4 }))).toBeNull();
  });

  it("carries pre-disconnect failures forward rather than losing them to the prompt", () => {
    const found = interruptionFromReport(
      options,
      report({
        sourceDisconnected: true,
        imported: 4,
        failures: [{ path: "/Volumes/Untitled/C7.MP4", error: "unreadable" }],
      }),
    );
    expect(found?.failed).toBe(1);
    expect(interruptedFailureNote(found!)).toContain("1 file failed");
    expect(interruptedFailureNote(interrupted())).toBe("");
  });

  it("leaves a run with per-file failures alone: that is ledger work, not hardware", () => {
    const failed = report({ imported: 4, failures: [{ path: "/Volumes/Untitled/C1.MP4", error: "unreadable" }] });
    expect(interruptionFromReport(options, failed)).toBeNull();
  });

  it("leaves a clean run alone", () => {
    expect(interruptionFromReport(options, report({ imported: 243 }))).toBeNull();
  });
});

describe("isSameInterruptedSession", () => {
  it("recognizes its own session across a remount at a new path", () => {
    expect(
      isSameInterruptedSession(interrupted(), {
        ...options,
        sourcePath: "/Volumes/Untitled 1",
        card: card({ mountPath: "/Volumes/Untitled 1" }),
      }),
    ).toBe(true);
  });

  it("does not let an unrelated card's clean run clear the interruption", () => {
    expect(
      isSameInterruptedSession(interrupted(), { ...options, card: card({ sessionId: "session_dji" }) }),
    ).toBe(false);
  });

  it("never conflates a card session with a plain folder at the same path", () => {
    expect(isSameInterruptedSession(interrupted(), { ...options, card: null })).toBe(false);
    expect(isSameInterruptedSession(interrupted({ card: null }), options)).toBe(false);
  });

  it("falls back to the path for two folder sessions", () => {
    const folder = { ...options, card: null };
    expect(isSameInterruptedSession(interrupted({ card: null }), folder)).toBe(true);
    expect(
      isSameInterruptedSession(interrupted({ card: null }), { ...folder, sourcePath: "/Volumes/Other" }),
    ).toBe(false);
  });
});

describe("matchesInterruptedSource", () => {
  it("recognizes the same card session", () => {
    expect(matchesInterruptedSource(interrupted(), card())).toBe(true);
  });

  it("rejects a different card mounted at the very same path", () => {
    const intruder = card({ sessionId: "session_other", label: "Untitled", mountPath: "/Volumes/Untitled" });
    expect(matchesInterruptedSource(interrupted(), intruder)).toBe(false);
  });

  it("recognizes the same card remounted at a different path", () => {
    expect(matchesInterruptedSource(interrupted(), card({ mountPath: "/Volumes/Untitled 1" }))).toBe(true);
  });

  it("never matches a plain-folder session, which has no card identity to check", () => {
    expect(matchesInterruptedSource(interrupted({ card: null }), card())).toBe(false);
    expect(detectsSourceAutomatically(interrupted({ card: null }))).toBe(false);
    expect(detectsSourceAutomatically(interrupted())).toBe(true);
  });

  it("matches nothing when no import is interrupted", () => {
    expect(matchesInterruptedSource(null, card())).toBe(false);
    expect(detectsSourceAutomatically(null)).toBe(false);
  });
});

describe("selectMountedCards", () => {
  it("recognizes the interrupted source even though reviewing it dismissed its banner", () => {
    const dismissed = new Set(["session_sony"]);
    const { banner, resumable } = selectMountedCards([card()], dismissed, interrupted());
    expect(resumable?.sessionId).toBe("session_sony");
    expect(banner).toBeNull();
  });

  it("keeps the interrupted source out of the banner so one card offers one action", () => {
    const other = card({ sessionId: "session_dji", label: "DJI_SD", mountPath: "/Volumes/DJI_SD" });
    const { banner, resumable } = selectMountedCards([card(), other], new Set(), interrupted());
    expect(resumable?.sessionId).toBe("session_sony");
    expect(banner?.sessionId).toBe("session_dji");
  });

  it("offers no resume for a different card at the interrupted mount path", () => {
    const intruder = card({ sessionId: "session_other" });
    const { banner, resumable } = selectMountedCards([intruder], new Set(), interrupted());
    expect(resumable).toBeNull();
    expect(banner?.sessionId).toBe("session_other");
  });

  it("behaves like plain mount detection when nothing is interrupted", () => {
    const dismissed = new Set(["session_sony"]);
    const other = card({ sessionId: "session_dji" });
    expect(selectMountedCards([card(), other], dismissed, null)).toEqual({
      banner: other,
      resumable: null,
    });
  });

  it("reports nothing when the slot is empty", () => {
    expect(selectMountedCards([], new Set(), interrupted())).toEqual({ banner: null, resumable: null });
  });
});

describe("disconnected copy", () => {
  it("leads with the camera family and keeps the volume name behind it", () => {
    expect(interruptedSourceHeadline(interrupted())).toBe(
      "Source disconnected — waiting for Sony — Volume Untitled",
    );
  });

  it("falls back to the volume name when the card has no camera hint", () => {
    expect(interruptedSourceLabel(interrupted({ card: card({ cameraHint: "  " }) }))).toBe("Volume Untitled");
  });

  it("names a plain folder by its path", () => {
    expect(interruptedSourceLabel(interrupted({ card: null, sourcePath: "/Volumes/NAS/Trip" }))).toBe(
      "/Volumes/NAS/Trip",
    );
  });

  it("tells the media menu whether the source is back or still missing", () => {
    expect(interruptedMenuNote(interrupted(), true)).toBe(
      "Sony — Volume Untitled · reconnected, ready to resume",
    );
    expect(interruptedMenuNote(interrupted(), false)).toBe(
      "Sony — Volume Untitled · waiting to reconnect",
    );
  });

  it("says what survived, so resuming is not mistaken for starting over", () => {
    expect(interruptedProgressNote(interrupted({ committed: 1 }))).toContain("1 clip committed");
    expect(interruptedProgressNote(interrupted({ committed: 12 }))).toContain("12 clips committed");
    expect(interruptedProgressNote(interrupted({ committed: 0 }))).toContain("Nothing was committed");
  });
});
