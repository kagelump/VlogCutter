/**
 * The decisions behind the paused import surface (#79), with nothing rendered.
 * Whether to continue on our own, keep waiting, or ask — and what to call the
 * volume meanwhile — are all answerable without a shell.
 */

import { describe, expect, it, vi } from "vitest";
import {
  correctiveFor,
  findPausedSource,
  pauseFromReport,
  pausedHeadline,
  probeOnce,
  resumeDecision,
  sourceProbe,
  unmetConditionDetail,
  type EndpointProbe,
  type PausedImport,
  type PausedImportOptions,
} from "./pausedImport";
import type { CardCandidate } from "../types";
import type { ImportRunReport } from "../types";

const options: PausedImportOptions = {
  projectId: "project_eclipse",
  sourcePath: "/Volumes/SD_Card",
  card: null,
  archiveRootId: "root_acasis",
  cameraOverride: null,
  verifyCopies: true,
  makeProxies: false,
  sourceLabel: "SD_Card",
  destinationLabel: "Acasis",
};

function card(sessionId: string): CardCandidate {
  return {
    sessionId,
    label: "SD_Card",
    mountPath: "/Volumes/SD_Card",
    cameraHint: "",
    clipCount: 12,
    totalBytes: 0,
  };
}

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
    session: {
      projectId: options.projectId,
      sourcePath: options.sourcePath,
      card: options.card,
      archiveRootId: options.archiveRootId,
      cameraOverride: options.cameraOverride,
      verifyCopies: options.verifyCopies,
      makeProxies: options.makeProxies,
    },
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

  it("names presence as the unmet condition while nothing is there", () => {
    expect(resumeDecision(paused(), { present: false, identityVerified: false })).toEqual({
      action: "blocked",
      unmet: ["endpoint-present"],
    });
  });

  /// The case that would otherwise write onto the wrong disk: something is
  /// there, and it cannot prove it is ours.
  it("names identity as the unmet condition when what returned cannot prove itself", () => {
    expect(resumeDecision(paused(), { present: true, identityVerified: false })).toEqual({
      action: "blocked",
      unmet: ["identity-proven"],
    });
  });

  /// Identity is not evaluable for something that is not there, so reporting it
  /// as unmet alongside presence would be noise rather than information.
  it("does not report identity as unmet when the endpoint is simply absent", () => {
    const decision = resumeDecision(paused(), { present: false, identityVerified: false });

    expect(decision).toEqual({ action: "blocked", unmet: ["endpoint-present"] });
  });

  /// Identity is never inferred from presence, whichever end it is.
  it("never continues on presence alone", () => {
    for (const endpoint of ["source", "destination"] as const) {
      expect(
        resumeDecision(paused({ endpoint }), { present: true, identityVerified: false }).action,
      ).not.toBe("continue");
    }
  });

  /// A pause is a set of conditions, not a stopwatch. Four hours of lid-shut is
  /// not a reason to stop, and a clock is not a safety property.
  it("decides the same way however long the pause has lasted", () => {
    const brief = paused({ pausedAt: 1_000 });
    const overnight = paused({ pausedAt: 1_000 - 8 * 60 * 60 * 1_000 });
    const probe = { present: true, identityVerified: true };

    expect(resumeDecision(overnight, probe)).toEqual(resumeDecision(brief, probe));
  });
});

describe("paused copy", () => {
  it("keeps the headline constant so the condition lines carry the news", () => {
    expect(pausedHeadline()).toBe("Import paused");
  });

  it("says a stalled volume is not responding, not that it is missing", () => {
    expect(
      unmetConditionDetail(paused({ reachability: "not-responding" }), "endpoint-present"),
    ).toBe("Acasis isn't responding");
  });

  it("says a vanished volume is not connected", () => {
    expect(unmetConditionDetail(paused({ reachability: "not-connected" }), "endpoint-present")).toBe(
      "Acasis isn't connected",
    );
  });

  it("says plainly when the drive that came back is a different one", () => {
    expect(unmetConditionDetail(paused(), "identity-proven")).toBe(
      "The drive connected as Acasis isn't the one this import was using",
    );
  });

  it("offers no corrective for a volume that is merely absent", () => {
    expect(correctiveFor(paused(), "endpoint-present")).toBeNull();
  });

  it("offers the smallest corrective for each endpoint that cannot prove itself", () => {
    expect(correctiveFor(paused({ endpoint: "destination" }), "identity-proven")).toBe(
      "locate-archive",
    );
    expect(correctiveFor(paused({ endpoint: "source" }), "identity-proven")).toBe("choose-source");
  });
});

/// Continuing is only possible if the pause kept everything the run was
/// started with. A paused import that forgot its destination could not go on.
describe("the paused session", () => {
  it("carries the choices the run was started with, so it can go on", () => {
    const state = pauseFromReport(options, report({ destinationDisconnected: true }), 1_000);

    expect(state?.session).toEqual({
      projectId: "project_eclipse",
      sourcePath: "/Volumes/SD_Card",
      card: null,
      archiveRootId: "root_acasis",
      cameraOverride: null,
      verifyCopies: true,
      makeProxies: false,
    });
  });
});

describe("sourceProbe", () => {
  it("is present and proven when the same card is back in the slot", () => {
    expect(sourceProbe(card("sess_a"), card("sess_a"))).toEqual({
      present: true,
      identityVerified: true,
    });
  });

  /// The source-side mirror of the destination escalation: a card is in the
  /// slot, and it is not the card this import was reading.
  it("is present but unproven when a different card is in the slot", () => {
    expect(sourceProbe(card("sess_a"), card("sess_b"))).toEqual({
      present: true,
      identityVerified: false,
    });
  });

  it("is absent when the slot is empty", () => {
    expect(sourceProbe(card("sess_a"), null)).toEqual({
      present: false,
      identityVerified: false,
    });
  });

  /// A plain folder has no identity to compare. It must not be waved through
  /// on presence alone — that is precisely the unproven case.
  it("never proves a source the run had no card identity for", () => {
    expect(sourceProbe(null, card("sess_b"))).toEqual({
      present: true,
      identityVerified: false,
    });
  });
});

/// #79 is explicit that a probe already in flight is never started twice. The
/// reason is #76 itself: probing a stalled volume can block for as long as the
/// import did, so a poll that fires every few seconds would otherwise pile up
/// one parked probe per tick against exactly the volume least able to answer.
describe("probeOnce", () => {
  it("runs the probe when none is in flight", async () => {
    const gate = probeOnce();
    const probe = vi.fn().mockResolvedValue({ present: true, identityVerified: true });

    expect(await gate(probe)).toEqual({ present: true, identityVerified: true });
    expect(probe).toHaveBeenCalledTimes(1);
  });

  it("skips a tick that lands while the previous probe is still parked", async () => {
    const gate = probeOnce();
    let release: (value: EndpointProbe) => void = () => {};
    const probe = vi.fn().mockReturnValue(new Promise<EndpointProbe>((r) => { release = r; }));

    const first = gate(probe);
    const second = await gate(probe);

    expect(second).toBeNull();
    expect(probe).toHaveBeenCalledTimes(1);
    release({ present: false, identityVerified: false });
    await first;
  });

  it("probes again once the parked probe finishes", async () => {
    const gate = probeOnce();
    const probe = vi.fn().mockResolvedValue({ present: false, identityVerified: false });

    await gate(probe);
    await gate(probe);

    expect(probe).toHaveBeenCalledTimes(2);
  });

  /// A probe that throws — a volume that errored rather than hung — must not
  /// leave the gate shut forever, or the pause could never recover.
  it("reopens the gate after a probe fails", async () => {
    const gate = probeOnce();
    const failing = vi.fn().mockRejectedValue(new Error("volume went away"));

    await expect(gate(failing)).rejects.toThrow("volume went away");
    const probe = vi.fn().mockResolvedValue({ present: true, identityVerified: true });
    expect(await gate(probe)).toEqual({ present: true, identityVerified: true });
  });
});

/// A volume that comes back while its old name is still taken remounts
/// somewhere else. #67 already established that the way back is identity, never
/// the pathname; a paused source has to be found the same way.
describe("findPausedSource", () => {
  it("finds the card by identity wherever it now sits", () => {
    const moved = { ...card("sess_a"), mountPath: "/Volumes/SD_Card 1" };

    expect(findPausedSource(card("sess_a"), [card("sess_b"), moved])).toBe(moved);
  });

  it("does not settle for a different card at the remembered path", () => {
    const intruder = { ...card("sess_b"), mountPath: "/Volumes/SD_Card" };

    expect(findPausedSource(card("sess_a"), [intruder])).toBeNull();
  });

  it("finds nothing when the run had no card identity to match on", () => {
    expect(findPausedSource(null, [card("sess_b")])).toBeNull();
  });
});
