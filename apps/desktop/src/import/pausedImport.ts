/**
 * Pausing an import whose volume stopped answering, and continuing it again
 * without asking (#79).
 *
 * #67 gave a vanished source a way back, but made the user press Resume,
 * because the old code could not tell a two-minute lid-close from an ejected
 * card. #76 removed that limitation: a stalled endpoint now fails within a
 * bounded time and says which end stopped answering, and #78 gave volumes an
 * identity, so "is my drive back?" is a question about hardware rather than
 * about whether something now occupies the folder it used to live in.
 *
 * What follows from those two is this module's whole premise: for the common
 * case — lid shut, lid opened, same drives — the user did nothing wrong and
 * nothing needs confirming. So the pause is a state to sit in, not a workflow
 * to drive. Its one routine action is Cancel; detection is automatic, with
 * nothing to press to hurry it along. It continues on its own the moment the
 * endpoint can *prove* it is the one we were using.
 *
 * That proof is the whole safety story, and it is why `resumeDecision` treats
 * "cannot identify" as unmet rather than as a match. A destination that cannot
 * prove itself is not a destination we may resume writing 138 GB onto.
 *
 * Continuing is a question about conditions, never about elapsed time. A pause
 * of four hours is judged exactly like a pause of four seconds — a clock is not
 * a safety property, and a threshold is a number nobody can pick correctly. So
 * the surface always answers the same question: what is still missing?
 */

import type { CardCandidate, ImportRunReport } from "../types";
import { isSameCardSession, type InterruptedImportOptions } from "./interruptedImport";

/** Which end of the copy stopped answering. */
export type PausedEndpoint = "source" | "destination";

/**
 * Whether the volume vanished or is still sitting there refusing to answer.
 *
 * Worth distinguishing in copy: the failure that prompted all of this was an
 * exFAT volume over FSKit, which stays mounted and listed while every read
 * hangs. Telling someone a drive "isn't connected" while its icon is in front
 * of them spends trust in the whole dialog.
 */
export type PausedReachability = "not-responding" | "not-connected";

/** An import waiting for a volume, holding everything needed to go on. */
export interface PausedImport {
  /**
   * The choices the run was started with. Continuing means continuing *this*
   * run — same destination, same camera, same verification the user agreed to
   * — so the pause keeps the whole snapshot rather than re-deriving it from a
   * dialog that is no longer open.
   */
  session: InterruptedImportOptions;
  endpoint: PausedEndpoint;
  /** What to call the volume while it is unavailable. */
  volumeLabel: string;
  reachability: PausedReachability;
  /** Clips this run committed before the pause. They stay committed. */
  committed: number;
  /**
   * Files that genuinely failed before the pause. A stall produces none — the
   * interrupted item is work still to do — so a non-zero count here is real
   * ledger work that must not vanish behind the paused surface.
   */
  failed: number;
  pausedAt: number;
}

/** What the shell learned on its last look at the missing endpoint. */
export interface EndpointProbe {
  /** Something is reachable at the endpoint again. */
  present: boolean;
  /**
   * That something proved it is the volume we were using — a UUID match, not
   * a pathname match (#78). False also covers "it is back but cannot identify
   * itself", which is not the same as a match and must not act like one.
   */
  identityVerified: boolean;
}

/**
 * Something that has to hold before the import goes on by itself.
 *
 * Deliberately not a stopwatch. Elapsed time is not one of these, because a
 * clock is not a safety property: a pause of four hours and a pause of four
 * seconds are the same question, and the answer is whichever of these is still
 * missing.
 */
export type ResumeCondition = "endpoint-present" | "identity-proven";

/** The smallest thing to offer a user whose endpoint cannot prove itself. */
export type Corrective = "locate-archive" | "choose-source";

/** What to do about a paused import, given the latest probe. */
export type ResumeDecision =
  | { action: "continue" }
  | { action: "blocked"; unmet: ResumeCondition[] };

/**
 * The naming and options a paused run has to restore when it continues.
 *
 * Extends #67's session snapshot rather than duplicating it: an import that
 * paused on its destination needs exactly what an import that lost its source
 * needed, plus the two labels the waiting surface says out loud.
 */
export interface PausedImportOptions extends InterruptedImportOptions {
  sourceLabel: string;
  destinationLabel: string;
}

/** The marker the core's bounded I/O uses when a volume stops answering (#76). */
const STALL_MARKER = "stopped answering";

/**
 * The paused state a finished run leaves behind, or null when the run ended
 * for any other reason.
 *
 * A cancelled run was the user's own decision, and an ordinary failed run is a
 * file-by-file problem for the ledger. Neither is waiting on hardware.
 */
export function pauseFromReport(
  options: PausedImportOptions,
  report: ImportRunReport,
  now: number = Date.now(),
): PausedImport | null {
  const { outcome } = report;
  const endpoint: PausedEndpoint | null = outcome.destinationDisconnected
    ? "destination"
    : outcome.sourceDisconnected
      ? "source"
      : null;
  if (endpoint === null) return null;

  // A stall leaves its fingerprint in the ledger: the core's bounded I/O says
  // so in the error text. Absent that, the volume was gone rather than mute.
  const stalled = outcome.failures.some((failure) => failure.error.includes(STALL_MARKER));

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
    endpoint,
    volumeLabel: endpoint === "destination" ? options.destinationLabel : options.sourceLabel,
    reachability: stalled ? "not-responding" : "not-connected",
    committed: outcome.imported + outcome.relinked,
    failed: outcome.failures.length,
    pausedAt: now,
  };
}

/**
 * Whether the import may continue on its own, keep waiting, or has to ask.
 *
 * The escalation case is deliberately narrow: something is there, and it
 * cannot prove it is ours.
 */
export function resumeDecision(_paused: PausedImport, probe: EndpointProbe): ResumeDecision {
  // Nothing is there. Identity is not evaluable for a volume that has not come
  // back, so reporting it as unmet too would be noise rather than information.
  if (!probe.present) return { action: "blocked", unmet: ["endpoint-present"] };
  // Present but unproven. Presence is not identity, and the cost of being wrong
  // here is writing onto someone else's disk.
  if (!probe.identityVerified) return { action: "blocked", unmet: ["identity-proven"] };
  return { action: "continue" };
}

/**
 * What the mount poll's answer means for a paused *source*.
 *
 * The destination's probe is a catalog question answered in the core, where
 * the archive root's recorded volume lives. A source is answered from the same
 * card listing the app already polls, so it costs nothing extra to ask.
 *
 * A run whose source was a plain folder has no identity to compare, so it can
 * never be proven — which is the honest answer, not a shortcoming. Presence is
 * not identity here either.
 */
export function sourceProbe(
  expected: CardCandidate | null,
  found: CardCandidate | null,
): EndpointProbe {
  return {
    present: found !== null,
    identityVerified: isSameCardSession(expected, found),
  };
}

/**
 * The card a paused source is waiting for, found wherever it is now mounted.
 *
 * By identity and only by identity. A volume that returns while its old name is
 * still taken comes back at a different path, so searching the remembered path
 * would miss the very case this exists for — and matching on that path would
 * accept a different card in the same slot, which is worse.
 */
export function findPausedSource(
  expected: CardCandidate | null,
  mounted: CardCandidate[],
): CardCandidate | null {
  if (!expected) return null;
  return mounted.find((candidate) => isSameCardSession(expected, candidate)) ?? null;
}

/**
 * A gate that lets one probe run at a time, skipping ticks that arrive while
 * one is still outstanding.
 *
 * #79 requires this for the same reason #76 exists: probing a volume that has
 * stopped answering can park for as long as the original copy did. A poll that
 * fired regardless would accumulate one stuck probe per tick against precisely
 * the volume least able to answer them — the failure mode being recovered from,
 * reproduced inside the recovery UI.
 *
 * A skipped tick returns null, which is not an answer and must not be read as
 * one: the last real probe stands until a new one lands.
 */
export function probeOnce<T = EndpointProbe>(): (probe: () => Promise<T>) => Promise<T | null> {
  let inFlight = false;
  return async (probe: () => Promise<T>) => {
    if (inFlight) return null;
    inFlight = true;
    try {
      return await probe();
    } finally {
      // Released on failure too. A volume that errors rather than hangs is
      // still a volume that may come back, and a gate left shut would mean the
      // pause could never end.
      inFlight = false;
    }
  };
}

/** The unchanging headline. What varies goes in the condition lines. */
export function pausedHeadline(): string {
  return "Import paused";
}

/** One line saying what this unmet condition means for this volume. */
export function unmetConditionDetail(
  paused: PausedImport,
  condition: ResumeCondition,
): string {
  if (condition === "endpoint-present") {
    const state = paused.reachability === "not-responding" ? "isn't responding" : "isn't connected";
    return `${paused.volumeLabel} ${state}`;
  }
  return `The drive connected as ${paused.volumeLabel} isn't the one this import was using`;
}

/**
 * The smallest corrective action for an unmet condition, or null when there is
 * nothing useful to offer.
 *
 * A volume that is merely absent needs no dialog — it needs plugging in, and
 * detection is automatic once it is. Only an endpoint that came back unable to
 * prove itself is a question a user can actually answer.
 */
export function correctiveFor(
  paused: PausedImport,
  condition: ResumeCondition,
): Corrective | null {
  if (condition !== "identity-proven") return null;
  return paused.endpoint === "destination" ? "locate-archive" : "choose-source";
}
