import type {
  AssetChronology,
  CaptureSequence,
  ChronologyImpact,
  ChronologyOverview,
  TripPhase,
} from "../chronologyTypes";

export interface ChronologyLane {
  sequence: CaptureSequence;
  assets: AssetChronology[];
}

export interface OrdinalWindow {
  start: number;
  end: number;
}

/** Groups the solver response without changing its order. */
export function chronologyLanes(overview: ChronologyOverview): ChronologyLane[] {
  const bySequence = new Map<string, AssetChronology[]>();
  for (const asset of overview.assets) {
    const id = asset.orderKey.captureSequenceId;
    // Append in place. Rebuilding the lane array per clip is quadratic, and an
    // expo-scale project puts thousands of clips on one card.
    const lane = bySequence.get(id);
    if (lane) lane.push(asset);
    else bySequence.set(id, [asset]);
  }
  const declared = overview.sequences.map((sequence) => ({
    sequence,
    assets: bySequence.get(sequence.id) ?? [],
  }));
  const declaredIds = new Set(overview.sequences.map((sequence) => sequence.id));
  const unknown = [...bySequence.entries()]
    .filter(([id]) => !declaredIds.has(id))
    .map(([id, assets]) => ({
      sequence: {
        id,
        cameraKey: "Unknown camera",
        label: "Unassigned clips",
        volumeHint: "",
        filenamePrefix: "",
      },
      assets,
    }));
  return [...declared, ...unknown].filter((lane) => lane.assets.length > 0);
}

export function ordinalWindow(
  total: number,
  zoom: number,
  scrollLeft: number,
  viewportWidth: number,
  overscanColumns = 8,
): OrdinalWindow {
  if (total <= 0 || zoom <= 0) return { start: 0, end: 0 };
  // Clamped to `total` as well as to 0: the result is always a valid slice, so
  // a caller cannot be handed a window that starts past the end of the list.
  const start = Math.min(total, Math.max(0, Math.floor(scrollLeft / zoom) - overscanColumns));
  const end = Math.min(
    total,
    Math.ceil((scrollLeft + Math.max(0, viewportWidth)) / zoom) + overscanColumns,
  );
  return { start, end: Math.max(start, end) };
}

export function contiguousRange(
  overview: ChronologyOverview,
  anchorAssetId: string,
  nextAssetId: string,
): string[] | null {
  const anchor = overview.assets.findIndex((asset) => asset.assetId === anchorAssetId);
  const next = overview.assets.findIndex((asset) => asset.assetId === nextAssetId);
  if (anchor < 0 || next < 0) return null;
  const start = Math.min(anchor, next);
  const end = Math.max(anchor, next);
  const range = overview.assets.slice(start, end + 1);
  const sequence = range[0]?.orderKey.captureSequenceId;
  if (!sequence || range.some((asset) => asset.orderKey.captureSequenceId !== sequence)) {
    return null;
  }
  return range.map((asset) => asset.assetId);
}

export function phaseOrdinalRange(
  phase: TripPhase,
  overview: ChronologyOverview,
): { start: number; end: number } | null {
  const start = phase.startAnchorAssetId
    ? overview.assets.findIndex((asset) => asset.assetId === phase.startAnchorAssetId)
    : overview.assets.findIndex((asset) => asset.displayTime?.phaseId === phase.id);
  let end = phase.endAnchorAssetId
    ? overview.assets.findIndex((asset) => asset.assetId === phase.endAnchorAssetId)
    : -1;
  if (end < 0) {
    for (let index = overview.assets.length - 1; index >= 0; index -= 1) {
      if (overview.assets[index]?.displayTime?.phaseId === phase.id) {
        end = index;
        break;
      }
    }
  }
  return start >= 0 && end >= start ? { start, end } : null;
}

export function changedOrdinalExtent(impact: ChronologyImpact | null) {
  if (!impact || impact.changes.length === 0) return null;
  const positions = impact.changes.flatMap((change) => [
    change.beforePosition,
    change.afterPosition,
  ]);
  return { start: Math.min(...positions), end: Math.max(...positions) };
}

/**
 * Renders the wall-clock value Rust already resolved for the trip phase.
 *
 * `displayTime.primary` deliberately carries a real RFC-3339 offset. Parsing it
 * into `Date` and formatting in the browser's timezone would turn a Tokyo clip
 * into the laptop's Madrid time. Read the serialized wall-clock fields instead;
 * Rust remains the only owner of timezone resolution and daylight-saving rules.
 */
export function formatChronologyTime(value: string | null, timezone?: string | null): string {
  if (!value) return "No camera time";
  const wallClock = value.match(
    /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(Z|[+-]\d{2}:\d{2})$/,
  );
  if (wallClock) {
    const [, year, month, day, hour, minute, second, offset] = wallClock;
    const monthName = new Intl.DateTimeFormat(undefined, { month: "short", timeZone: "UTC" })
      .format(new Date(Date.UTC(Number(year), Number(month) - 1, Number(day))));
    const zone = timezone?.trim() || (offset === "Z" ? "UTC" : `UTC${offset}`);
    return `${monthName} ${Number(day)}, ${hour}:${minute}:${second} · ${zone}`;
  }
  const parsed = new Date(value);
  if (Number.isNaN(parsed.valueOf())) return value;
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(parsed);
}
