import type {
  AssetChronology,
  CaptureSequence,
  ChronologyImpact,
  ChronologyOverview,
  TripPhase,
} from "../chronologyTypes";

export interface ChronologyLane {
  cameraKey: string;
  sequences: Array<{
    sequence: CaptureSequence;
    assets: AssetChronology[];
    /** Contiguous runs in this camera lane; other cameras do not break a run. */
    runs: Array<{
      assets: AssetChronology[];
      startIndex: number;
      endIndex: number;
    }>;
  }>;
  assets: AssetChronology[];
}

export interface OrdinalWindow {
  start: number;
  end: number;
}

/**
 * Groups the solver response into physical-camera lanes and card segments.
 *
 * A capture sequence protects one card's filename counters and clock facts;
 * it is not another camera. The lane order and each card's left-to-right order
 * follow the solver's first placed clip, matching Review's camera-first ribbon.
 */
export function chronologyLanes(overview: ChronologyOverview): ChronologyLane[] {
  const bySequence = new Map<string, AssetChronology[]>();
  const indexByAssetId = new Map(
    overview.assets.map((asset, index) => [asset.assetId, index]),
  );
  for (const asset of overview.assets) {
    const id = asset.orderKey.captureSequenceId;
    // Append in place. Rebuilding the lane array per clip is quadratic, and an
    // expo-scale project puts thousands of clips on one card.
    const lane = bySequence.get(id);
    if (lane) lane.push(asset);
    else bySequence.set(id, [asset]);
  }
  const declaredIds = new Set(overview.sequences.map((sequence) => sequence.id));
  const unknownSequences: CaptureSequence[] = [...bySequence.entries()]
    .filter(([id]) => !declaredIds.has(id))
    .map(([id]) => ({
      id,
      cameraKey: "Unknown camera",
      label: "Unassigned clips",
      volumeHint: "",
      filenamePrefix: "",
    }));

  const segments = [...overview.sequences, ...unknownSequences]
    .map((sequence) => {
      const assets = bySequence.get(sequence.id) ?? [];
      return {
        sequence,
        assets,
        runs: [] as ChronologyLane["sequences"][number]["runs"],
      };
    })
    .filter((segment) => segment.assets.length > 0)
    .sort((left, right) => (
      (indexByAssetId.get(left.assets[0].assetId) ?? 0)
      - (indexByAssetId.get(right.assets[0].assetId) ?? 0)
    ));

  const byCamera = new Map<string, ChronologyLane>();
  for (const segment of segments) {
    const cameraKey = segment.sequence.cameraKey.trim() || "Unknown camera";
    const lane = byCamera.get(cameraKey);
    if (lane) {
      lane.sequences.push(segment);
      lane.assets.push(...segment.assets);
    } else {
      byCamera.set(cameraKey, {
        cameraKey,
        sequences: [segment],
        assets: [...segment.assets],
      });
    }
  }
  for (const lane of byCamera.values()) {
    lane.assets.sort(
      (left, right) => (indexByAssetId.get(left.assetId) ?? 0) - (indexByAssetId.get(right.assetId) ?? 0),
    );
    const segmentById = new Map(lane.sequences.map((segment) => [segment.sequence.id, segment]));
    let previousSequenceId: string | null = null;
    for (const asset of lane.assets) {
      const sequenceId = asset.orderKey.captureSequenceId;
      const segment = segmentById.get(sequenceId);
      if (!segment) continue;
      const index = indexByAssetId.get(asset.assetId) ?? 0;
      const activeRun = sequenceId === previousSequenceId
        ? segment.runs[segment.runs.length - 1]
        : null;
      if (activeRun) {
        activeRun.assets.push(asset);
        activeRun.endIndex = index;
      } else {
        segment.runs.push({ assets: [asset], startIndex: index, endIndex: index });
      }
      previousSequenceId = sequenceId;
    }
  }
  return [...byCamera.values()];
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

const WALL_CLOCK =
  /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(Z|[+-]\d{2}:\d{2})$/;

interface WallClock {
  year: string;
  month: string;
  day: string;
  hour: string;
  minute: string;
  second: string;
  offset: string;
}

/**
 * Splits a solver timestamp into the fields it literally carries.
 *
 * Deliberately not `Date`: see `formatChronologyTime` below for why the
 * serialized wall clock is the only trustworthy reading of these values.
 */
function parseWallClock(value: string | null): WallClock | null {
  const found = value?.match(WALL_CLOCK);
  if (!found) return null;
  const [, year, month, day, hour, minute, second, offset] = found;
  return { year, month, day, hour, minute, second, offset };
}

/** "Feb 8", from wall-clock fields rather than a reparsed instant. */
function formatWallClockDate(clock: WallClock): string {
  const monthName = new Intl.DateTimeFormat(undefined, { month: "short", timeZone: "UTC" })
    .format(new Date(Date.UTC(Number(clock.year), Number(clock.month) - 1, Number(clock.day))));
  return `${monthName} ${Number(clock.day)}`;
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
  const clock = parseWallClock(value);
  if (clock) {
    const zone = timezone?.trim() || (clock.offset === "Z" ? "UTC" : `UTC${clock.offset}`);
    return `${formatWallClockDate(clock)}, ${clock.hour}:${clock.minute}:${clock.second} · ${zone}`;
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

/** Shorter form for the ruler: no seconds, no zone, no date. */
export function formatChronologyClock(value: string | null): string | null {
  const clock = parseWallClock(value);
  return clock ? `${clock.hour}:${clock.minute}` : null;
}

export interface TimeTick {
  /** The ordinal column the tick is drawn at. */
  index: number;
  /** "Feb 8" at a day boundary, "14:31" otherwise. */
  label: string;
  isDayBoundary: boolean;
  /**
   * The city the clips from here on are displayed in, set only where that
   * changes. Without it the ruler is a liar on any trip that crosses zones:
   * a Tokyo-to-Madrid flight makes the wall clock jump *backwards* across the
   * transit, and a reader with no marker there sees the solver's order
   * contradicting itself.
   */
  zone: string | null;
}

/** "Europe/Madrid" → "Madrid". Left alone when there is no region prefix. */
function shortZone(timezone: string | null | undefined): string | null {
  const trimmed = timezone?.trim();
  if (!trimmed) return null;
  return (trimmed.split("/").pop() ?? trimmed).replace(/_/g, " ");
}

/**
 * Date and time markers for the ruler above the lanes.
 *
 * The ruler exists because the tiles no longer carry a timestamp: they carry a
 * frame. Elapsed time therefore has to be legible from the canvas itself, and a
 * per-clip label at 68px columns is exactly the truncated mess the tiles were.
 *
 * Two kinds of tick. Regular ones land on absolute multiples of a stride
 * derived from `minSpacingPx`, so they stay put while the window scrolls over
 * them rather than sliding about. Forced ticks are emitted wherever the
 * calendar date or the display timezone changes, however close to a regular
 * tick they fall: those two are the most useful things this ruler can say, and
 * they always win the collision.
 *
 * Only `assets` inside `window` are considered, but the comparisons reach one
 * clip further back so a change is not missed at the window's left edge.
 */
export function chronologyTimeTicks(
  assets: AssetChronology[],
  columnWidth: number,
  window: OrdinalWindow,
  minSpacingPx = 140,
): TimeTick[] {
  if (columnWidth <= 0 || assets.length === 0) return [];
  const stride = Math.max(1, Math.ceil(minSpacingPx / columnWidth));
  const clockAt = (index: number) =>
    parseWallClock(assets[index]?.displayTime?.primary ?? assets[index]?.tripTime ?? null);
  const dayAt = (index: number) => {
    const clock = clockAt(index);
    return clock ? `${clock.year}-${clock.month}-${clock.day}` : null;
  };

  const ticks: TimeTick[] = [];
  let previousDay: string | null = null;
  let previousZone: string | null = null;
  // Walk back for the last placed clip before the window, so the first visible
  // tick knows whether it is opening a new day or continuing one.
  for (let index = Math.min(window.start, assets.length) - 1; index >= 0; index -= 1) {
    previousDay = dayAt(index);
    if (previousDay) {
      previousZone = shortZone(assets[index]?.displayTime?.timezone);
      break;
    }
  }
  let firstInWindow = true;

  for (let index = Math.max(0, window.start); index < Math.min(window.end, assets.length); index += 1) {
    const clock = clockAt(index);
    if (!clock) continue;
    const day = `${clock.year}-${clock.month}-${clock.day}`;
    const zone = shortZone(assets[index]?.displayTime?.timezone);
    // The window's own first placed clip is dated and named too: a ruler that
    // opened on a bare "14:31" would leave the user guessing which day — and
    // which city — they scrolled into.
    const isDayBoundary = day !== previousDay || firstInWindow;
    const zoneChanged = zone !== previousZone || firstInWindow;
    previousDay = day;
    previousZone = zone;
    firstInWindow = false;

    const last = ticks[ticks.length - 1];
    if (isDayBoundary || zoneChanged) {
      // A forced tick displaces a regular one sitting right on top of it.
      if (last && index - last.index < stride && !last.isDayBoundary && !last.zone) ticks.pop();
      ticks.push({
        index,
        label: isDayBoundary ? formatWallClockDate(clock) : `${clock.hour}:${clock.minute}`,
        isDayBoundary,
        zone: zoneChanged ? zone : null,
      });
      continue;
    }
    if (index % stride !== 0) continue;
    if (last && index - last.index < stride) continue;
    ticks.push({ index, label: `${clock.hour}:${clock.minute}`, isDayBoundary: false, zone: null });
  }
  return ticks;
}
