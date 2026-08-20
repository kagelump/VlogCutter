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
 * to drive. It shows what is blocking progress, offers exactly two routine
 * actions, and continues on its own the moment the endpoint can *prove* it is
 * the one we were using.
 *
 * That proof is the whole safety story, and it is why `resumeDecision` treats
 * "cannot identify" as an escalation rather than a match. A destination that
 * cannot prove itself is not a destination we may resume writing 138 GB onto.
 */

import type { ImportRunReport } from "../types";

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

/** What to do about a paused import, given the latest probe. */
export type ResumeDecision =
  | { action: "continue" }
  | { action: "keep-waiting" }
  | { action: "escalate"; corrective: "locate-archive" | "choose-source" };

/** The naming and options a paused run has to restore when it continues. */
export interface PausedImportOptions {
  projectId: string;
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
export function resumeDecision(paused: PausedImport, probe: EndpointProbe): ResumeDecision {
  if (!probe.present) return { action: "keep-waiting" };
  if (probe.identityVerified) return { action: "continue" };
  // Present but unproven. Presence is not identity, and the cost of being
  // wrong here is writing onto someone else's disk, so this is the one place
  // the pause turns into a question.
  return {
    action: "escalate",
    corrective: paused.endpoint === "destination" ? "locate-archive" : "choose-source",
  };
}

/** The unchanging headline. What varies goes in the detail line. */
export function pausedHeadline(): string {
  return "Import paused";
}

/** One line naming the volume and what is wrong with it. */
export function pausedDetail(paused: PausedImport): string {
  const state = paused.reachability === "not-responding" ? "isn't responding" : "isn't connected";
  return `${paused.volumeLabel} ${state}`;
}
