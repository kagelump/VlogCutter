/**
 * Chronology fixtures.
 *
 * These *state* an outcome; they do not derive one. Given a declared order of
 * clips, `buildChronologyOverview` produces the DTO the backend would return
 * for that order — it never decides what the order should be. Deciding is the
 * solver's job, and `cargo test -p rcdvlog-core` is where that decision is
 * checked.
 */

import type {
  AssetChronology,
  CaptureSequence,
  ChronologyOverview,
  ClockEpoch,
  Contradiction,
  ContradictionKind,
  Diagnostic,
  DiagnosticSeverity,
  DeclinedSplit,
  ProposedSplit,
  TripPhase,
} from "../../chronologyTypes";
import { EMPTY_CHRONOLOGY_OVERVIEW } from "./state";

export interface SequenceSpec {
  id: string;
  cameraKey: string;
  label?: string;
  volumeHint?: string;
  filenamePrefix?: string;
  /** Clip identities the backend assigned to this sequence. */
  assetIds?: string[];
}

export interface ChronologySpec {
  /** The clips, in the order the solver placed them. Declared, not computed. */
  order: string[];
  sequences?: SequenceSpec[];
  epochs?: ClockEpoch[];
  phases?: TripPhase[];
  contradictions?: Contradiction[];
  proposedSplits?: ProposedSplit[];
  declinedSplits?: DeclinedSplit[];
  unplacedAssets?: number;
  /** First clip's placed time; later clips follow at `spacingSeconds`. */
  startAt?: string;
  spacingSeconds?: number;
  timezone?: string;
}

export function buildSequence(spec: SequenceSpec): CaptureSequence {
  return {
    id: spec.id,
    cameraKey: spec.cameraKey,
    label: spec.label ?? spec.cameraKey,
    volumeHint: spec.volumeHint ?? "",
    filenamePrefix: spec.filenamePrefix ?? "",
  };
}

/**
 * `phases::covering_phase`'s rule, in the harness: inclusive at both ends, the
 * later `startTripTime` wins an overlap, and an exact tie goes to the greater
 * id.
 */
function coveringPhase(phases: TripPhase[], at: number): TripPhase | null {
  let best: TripPhase | null = null;
  for (const phase of phases) {
    const from = Date.parse(phase.startTripTime);
    const to = Date.parse(phase.endTripTime);
    if (Number.isNaN(from) || Number.isNaN(to) || at < from || at > to) continue;
    if (!best) { best = phase; continue; }
    const bestFrom = Date.parse(best.startTripTime);
    if (from > bestFrom || (from === bestFrom && phase.id > best.id)) best = phase;
  }
  return best;
}

/** The zone a phase displays in. Transit shows its origin first, as Rust does. */
function phaseTimezone(phase: TripPhase | null): string | null {
  if (!phase) return null;
  const zone = phase.kind === "transit"
    ? phase.originTimezone ?? phase.timezone
    : phase.timezone;
  return zone?.trim() || null;
}

/**
 * Re-derive every clip's display time from the phases now in the overview.
 *
 * The real backend recomputes this on any phase write, because display
 * timezone is a *function* of phase coverage rather than a field stored per
 * clip. Without this the harness would let a saved timezone appear in the
 * phase list while the readout beside it still said UTC — a disagreement the
 * production code cannot produce and a CUJ must not be able to pass through.
 */
export function withPhaseDisplayTimes(overview: ChronologyOverview): ChronologyOverview {
  return {
    ...overview,
    assets: overview.assets.map((asset) => {
      const at = Date.parse(asset.tripTime ?? "");
      const primary = asset.displayTime?.primary ?? asset.tripTime;
      // A clip with no placeable time has nothing for a phase to cover.
      if (Number.isNaN(at) || primary === null) return asset;
      const covering = coveringPhase(overview.phases, at);
      return {
        ...asset,
        displayTime: {
          primary,
          secondary: asset.displayTime?.secondary ?? null,
          timezone: phaseTimezone(covering) ?? "UTC",
          phaseId: covering?.id ?? null,
        },
      };
    }),
  };
}

export function buildChronologyOverview(spec: ChronologySpec): ChronologyOverview {
  const sequences = (spec.sequences ?? []).map(buildSequence);
  const sequenceByAssetId = new Map<string, string>();
  for (const sequence of spec.sequences ?? []) {
    for (const assetId of sequence.assetIds ?? []) {
      sequenceByAssetId.set(assetId, sequence.id);
    }
  }
  const start = Date.parse(spec.startAt ?? "2026-02-08T00:10:00.000Z");
  const spacing = (spec.spacingSeconds ?? 300) * 1000;
  const phases = spec.phases ?? [];

  const assets: AssetChronology[] = spec.order.map((assetId, index) => {
    const placedAt = start + index * spacing;
    const tripTime = new Date(placedAt).toISOString();
    // Local display time comes from the covering phase or it does not exist —
    // the same coupling `phases::display_time` enforces. A fixture that handed
    // out a named zone with no phase behind it made an uncovered clip look
    // answered, which is exactly the state #72's readout has to distinguish.
    const covering = coveringPhase(phases, placedAt);
    return {
      assetId,
      rawTime: tripTime,
      tripTime,
      displayTime: {
        primary: tripTime,
        secondary: null,
        timezone: phaseTimezone(covering) ?? spec.timezone ?? "UTC",
        phaseId: covering?.id ?? null,
      },
      confidence: 0.9,
      evidence: [{ kind: "camera_clock", detail: "camera clock", weight: 1 }],
      uncertaintySeconds: 0,
      orderKey: {
        tripTimeMillis: start + index * spacing,
        captureSequenceId: sequenceByAssetId.get(assetId) ?? sequences[0]?.id ?? "sequence_1",
        filenameCounter: index + 1,
        filename: assetId,
        assetId,
      },
    };
  });

  return {
    ...EMPTY_CHRONOLOGY_OVERVIEW,
    sequences,
    epochs: spec.epochs ?? [],
    phases,
    contradictions: spec.contradictions ?? [],
    proposedSplits: spec.proposedSplits ?? [],
    declinedSplits: spec.declinedSplits ?? [],
    unplacedAssets: spec.unplacedAssets ?? 0,
    assets,
  };
}

/** Stable request keys for declared fake-backend responses. */
export function visualPlacementRequestKey(
  assetIds: string[],
  targetAssetId: string,
  side: "before" | "after",
): string {
  return `${assetIds.join("\u0000")}|${side}|${targetAssetId}`;
}

export function reassignmentPreviewRequestKey(anchorAssetId: string, intoSequenceId: string): string {
  return `${anchorAssetId}|${intoSequenceId}`;
}

export function reassignmentWriteRequestKey(
  fromSequenceId: string,
  intoSequenceId: string,
  assetIds: string[],
): string {
  return `${fromSequenceId}|${intoSequenceId}|${assetIds.join("\u0000")}`;
}

export function sequenceSplitRequestKey(
  sourceSequenceId: string,
  atCounter: number,
  assetIds: string[],
): string {
  return `${sourceSequenceId}|${atCounter}|${assetIds.join("\u0000")}`;
}

export function sequenceMergeRequestKey(fromSequenceId: string, intoSequenceId: string): string {
  return `${fromSequenceId}|${intoSequenceId}`;
}

/** The order the overview declares, as the CUJs read it back. */
export function chronologyOrder(overview: ChronologyOverview): string[] {
  return overview.assets.map((asset) => asset.assetId);
}

export interface ContradictionSpec {
  kind?: ContradictionKind;
  strength?: "hard" | "soft";
  assetIds?: string[];
  message: string;
  evidence?: string[];
}

export function buildContradiction(spec: ContradictionSpec): Contradiction {
  return {
    kind: spec.kind ?? "interval_conflict",
    strength: spec.strength ?? "hard",
    assetIds: spec.assetIds ?? [],
    evidence: (spec.evidence ?? []).map((detail) => ({ kind: "user_set", detail, weight: 1 })),
    message: spec.message,
  };
}

export interface DiagnosticSpec {
  id?: string;
  severity?: DiagnosticSeverity;
  kind?: string;
  message: string;
  volumeHint?: string;
  assetIds?: string[];
  occurrenceCount?: number;
  seenAt?: string;
}

export function buildDiagnostic(spec: DiagnosticSpec): Diagnostic {
  const kind = spec.kind ?? "media.volume_offline";
  const id = spec.id ?? `diagnostic_${kind.replace(/\W+/g, "_")}`;
  const seenAt = spec.seenAt ?? "2026-08-09T12:00:00.000Z";
  return {
    id,
    dedupeKey: id,
    severity: spec.severity ?? "warning",
    kind,
    subject: spec.volumeHint ? { type: "volume", volumeHint: spec.volumeHint } : { type: "project" },
    message: spec.message,
    context: { volumeHint: spec.volumeHint ?? "", assetIds: spec.assetIds ?? [] },
    occurrenceCount: spec.occurrenceCount ?? 1,
    firstSeenAt: seenAt,
    lastSeenAt: seenAt,
    acknowledgedAt: null,
  };
}
