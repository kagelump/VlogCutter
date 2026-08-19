/**
 * Recovering a Smart ingest session whose source went away mid-import (#67).
 *
 * A removable source disappearing is a normal field failure — a bus-powered
 * enclosure browns out, a cable is nudged, a card is pulled a second early —
 * and the architecture's durability model already anticipates it: every file
 * commits atomically, so an interruption leaves committed work committed and
 * the remainder simply undone (`docs/tasks/import/ARCHITECTURE.md` §4.4).
 *
 * What was missing was the way back. This module holds the session identity
 * and the user's choices across the gap, and answers the two questions the
 * shell asks on every mount poll: is this the source that went away, and is
 * anything left worth showing in the mount banner. Both answers are decided
 * on card *session identity*, never on a pathname — see `matchesInterruptedSource`.
 */

import type { CardCandidate, ImportRunReport } from "../types";

/** Everything needed to reopen one interrupted Smart ingest session. */
export interface InterruptedImport {
  /** The project the interrupted run belonged to. Resume is scoped to it. */
  projectId: string;
  sourcePath: string;
  /** Card identity when the source was a card; null for a plain folder. */
  card: CardCandidate | null;
  /** The destination the user chose, preserved across the reconnect. */
  archiveRootId: string;
  cameraOverride: string | null;
  verifyCopies: boolean;
  makeProxies: boolean;
  /** Clips this run committed before the source went away. */
  committed: number;
  /**
   * Files that genuinely failed *before* the source left. The disconnection
   * itself produces none — the interrupted file copied nothing and is simply
   * work still to do — so a non-zero count here is real ledger work that must
   * not disappear behind the reconnect prompt.
   */
  failed: number;
  interruptedAt: number;
}

/** The options an interrupted session restores when it is resumed. */
export interface InterruptedImportOptions {
  projectId: string;
  sourcePath: string;
  card: CardCandidate | null;
  archiveRootId: string;
  cameraOverride: string | null;
  verifyCopies: boolean;
  makeProxies: boolean;
}

/**
 * The interrupted session a finished run leaves behind, or null when the run
 * ended for any other reason.
 *
 * Only `sourceDisconnected` produces one. A cancelled run was the user's own
 * decision and a failed run is a file-by-file problem for the ledger; neither
 * is waiting for hardware to come back.
 */
export function interruptionFromReport(
  options: InterruptedImportOptions,
  report: ImportRunReport,
  now: number = Date.now(),
): InterruptedImport | null {
  if (!report.outcome.sourceDisconnected) return null;
  return {
    ...options,
    committed: report.outcome.imported + report.outcome.relinked,
    failed: report.outcome.failures.length,
    interruptedAt: now,
  };
}

/**
 * Whether a finished run belonged to the session currently recorded as
 * interrupted, so a clean completion clears only its own recovery state.
 *
 * Matched on card identity when there is one: a resume legitimately re-enters
 * at a new mount path, and the pathname is the one thing that can change while
 * the session stays the same.
 */
export function isSameInterruptedSession(
  interrupted: InterruptedImport,
  options: InterruptedImportOptions,
): boolean {
  if (interrupted.card && options.card) {
    return interrupted.card.sessionId === options.card.sessionId;
  }
  if (interrupted.card || options.card) return false;
  return interrupted.sourcePath === options.sourcePath;
}

/**
 * Whether a mounted card is the interrupted session's own source.
 *
 * Session identity only. An enclosure can drop a volume and remount a
 * different card at the same `/Volumes/Untitled` within seconds, and resuming
 * into that card would copy the wrong footage under the interrupted session's
 * archive path. A session whose source was a plain folder has no card identity
 * to check and is never matched here; it reconnects by re-running preflight.
 */
export function matchesInterruptedSource(
  interrupted: InterruptedImport | null,
  card: CardCandidate | null,
): boolean {
  return isSameCardSession(interrupted?.card ?? null, card);
}

/**
 * The identity comparison itself, for callers holding a card rather than a
 * recorded session — asking of a source mid-run whether it is still the one
 * the plan was built against.
 */
export function isSameCardSession(
  expected: CardCandidate | null,
  found: CardCandidate | null,
): boolean {
  if (!expected || !found) return false;
  return expected.sessionId === found.sessionId;
}

/** Whether the mount poll can recognize this session's source on its own. */
export function detectsSourceAutomatically(interrupted: InterruptedImport | null): boolean {
  return Boolean(interrupted?.card);
}

export interface MountPollResult {
  /** The card the mount banner should offer, if any. */
  banner: CardCandidate | null;
  /** The interrupted session's own source, back in the slot. */
  resumable: CardCandidate | null;
}

/**
 * Splits one mount poll into the banner's card and the interrupted session's.
 *
 * The dismissed set deliberately does not apply to the interrupted source.
 * Choosing to review a card adds it to that set, so an import that then loses
 * its card would otherwise be unable to notice the card coming back — the
 * exact recovery trap #67 describes. Dismissal is a statement about a banner
 * the user did not want, not about a session they started.
 */
export function selectMountedCards(
  cards: CardCandidate[],
  dismissed: ReadonlySet<string>,
  interrupted: InterruptedImport | null,
): MountPollResult {
  const resumable = cards.find((card) => matchesInterruptedSource(interrupted, card)) ?? null;
  const banner = cards.find(
    (card) => !dismissed.has(card.sessionId) && card.sessionId !== resumable?.sessionId,
  ) ?? null;
  return { banner, resumable };
}

/** How the source names itself: camera family first, volume name behind it. */
export function interruptedSourceLabel(interrupted: InterruptedImport): string {
  if (!interrupted.card) return interrupted.sourcePath;
  const camera = interrupted.card.cameraHint.trim();
  const volume = `Volume ${interrupted.card.label}`;
  return camera ? `${camera} — ${volume}` : volume;
}

/** The one line the disconnected state leads with. */
export function interruptedSourceHeadline(interrupted: InterruptedImport): string {
  return `Source disconnected — waiting for ${interruptedSourceLabel(interrupted)}`;
}

/** What the interrupted run already made durable, for the reconnect copy. */
export function interruptedProgressNote(interrupted: InterruptedImport): string {
  if (interrupted.committed === 0) {
    return "Nothing was committed before the source went away, so resuming starts from the top.";
  }
  const clips = `${interrupted.committed} clip${interrupted.committed === 1 ? "" : "s"}`;
  return `${clips} committed before the source went away. Resuming copies only what is left.`;
}

/** Files that failed on their own merits before the source left, if any. */
export function interruptedFailureNote(interrupted: InterruptedImport): string {
  if (interrupted.failed === 0) return "";
  const files = `${interrupted.failed} file${interrupted.failed === 1 ? "" : "s"}`;
  return `${files} failed before the source left and still need attention in the import ledger.`;
}

/** The media-menu entry's second line: which source, and whether it is back. */
export function interruptedMenuNote(interrupted: InterruptedImport, reconnected: boolean): string {
  const label = interruptedSourceLabel(interrupted);
  return reconnected ? `${label} · reconnected, ready to resume` : `${label} · waiting to reconnect`;
}
