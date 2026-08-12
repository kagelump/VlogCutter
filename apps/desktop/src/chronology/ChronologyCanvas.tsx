import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import type { Asset } from "../types";
import type { ChronologyImpact, ChronologyOverview, TripPhase } from "../chronologyTypes";
import {
  changedOrdinalExtent,
  chronologyLanes,
  contiguousRange,
  formatChronologyTime,
  ordinalWindow,
  phaseOrdinalRange,
} from "./geometry";

export interface PhaseBoundaryDraft {
  phase: TripPhase;
  startIndex: number;
  endIndex: number;
}

interface BoundaryDrag {
  phase: TripPhase;
  edge: "start" | "end";
  startIndex: number;
  endIndex: number;
  changed: boolean;
}

interface Props {
  overview: ChronologyOverview;
  assetsById: Map<string, Asset>;
  focusedAssetId: string | null;
  selectedIds: string[];
  impact: ChronologyImpact | null;
  columnWidth?: number;
  initialScrollLeft?: number;
  disabled?: boolean;
  onFocusAsset(assetId: string): void;
  onSelectRange(assetIds: string[]): void;
  onPlaceSelection(targetAssetId: string, side: "before" | "after"): void;
  onAdjustPhase(draft: PhaseBoundaryDraft): void;
}

const COLUMN = 92;
const GUTTER = 160;

/**
 * Only used until the container reports its own width, and in engines with no
 * `ResizeObserver`. It is deliberately generous: painting a few columns nobody
 * looks at costs one frame, while painting too few leaves a maximised desk
 * visibly blank down its right-hand side.
 */
const FALLBACK_VIEWPORT = 2560;

export function ChronologyCanvas({
  overview,
  assetsById,
  focusedAssetId,
  selectedIds,
  impact,
  columnWidth = COLUMN,
  initialScrollLeft,
  disabled = false,
  onFocusAsset,
  onSelectRange,
  onPlaceSelection,
  onAdjustPhase,
}: Props) {
  const lanes = useMemo(() => chronologyLanes(overview), [overview]);
  const indexById = useMemo(
    () => new Map(overview.assets.map((asset, index) => [asset.assetId, index])),
    [overview.assets],
  );
  const scrollRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLDivElement>(null);
  const selectionAnchor = useRef<string | null>(null);
  const boundaryDragRef = useRef<BoundaryDrag | null>(null);
  const suppressBoundaryClick = useRef(false);
  const skipInitialCenterRef = useRef(initialScrollLeft !== undefined);
  const [viewport, setViewport] = useState({ left: 0, width: FALLBACK_VIEWPORT });
  const [boundaryDrag, setBoundaryDrag] = useState<BoundaryDrag | null>(null);
  const visibleWindow = ordinalWindow(
    overview.assets.length,
    columnWidth,
    Math.max(0, viewport.left - GUTTER),
    viewport.width,
    6,
  );
  const visibleIds = useMemo(
    () => new Set(overview.assets.slice(visibleWindow.start, visibleWindow.end).map((asset) => asset.assetId)),
    [overview.assets, visibleWindow.end, visibleWindow.start],
  );
  const selected = new Set(selectedIds);
  const impactExtent = changedOrdinalExtent(impact);

  useEffect(() => {
    // The desk opens with the focused problem/Review clip preselected. Make it
    // the keyboard/mouse range anchor too; otherwise the first Shift-click
    // silently behaves like an ordinary click.
    if (selectedIds.length === 1) selectionAnchor.current = selectedIds[0];
    else if (selectedIds.length === 0) selectionAnchor.current = null;
  }, [selectedIds]);

  // Measure the real container rather than assuming a width. Without this the
  // virtual window is sized from the fallback alone, so on any desk wider than
  // it the right-hand columns stay unpainted until the first scroll event —
  // and a canvas opened on its first clip never gets one.
  useEffect(() => {
    const scroll = scrollRef.current;
    if (!scroll) return;
    const measure = () => setViewport({
      left: scroll.scrollLeft,
      width: scroll.clientWidth || FALLBACK_VIEWPORT,
    });
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(scroll);
    return () => observer.disconnect();
  }, []);

  useLayoutEffect(() => {
    const scroll = scrollRef.current;
    if (initialScrollLeft === undefined || !scroll) return;
    scroll.scrollLeft = Math.max(0, initialScrollLeft);
    setViewport({ left: scroll.scrollLeft, width: scroll.clientWidth || FALLBACK_VIEWPORT });
  }, [initialScrollLeft]);

  useEffect(() => {
    if (skipInitialCenterRef.current) {
      skipInitialCenterRef.current = false;
      return;
    }
    const index = focusedAssetId ? indexById.get(focusedAssetId) : undefined;
    const scroll = scrollRef.current;
    if (index === undefined || !scroll) return;
    const left = index * columnWidth + GUTTER - scroll.clientWidth / 2;
    scroll.scrollTo({ left: Math.max(0, left), behavior: "smooth" });
  }, [columnWidth, focusedAssetId, indexById]);

  function choose(assetId: string, extend: boolean) {
    onFocusAsset(assetId);
    if (!extend || !selectionAnchor.current) {
      selectionAnchor.current = assetId;
      onSelectRange([assetId]);
      return;
    }
    const range = contiguousRange(overview, selectionAnchor.current, assetId);
    if (range) {
      onSelectRange(range);
      return;
    }
    // A run cannot cross capture sequences. Keep focus and selection aligned
    // by starting a fresh run at the clicked clip instead of leaving an old
    // card selected under a new inspector focus.
    selectionAnchor.current = assetId;
    onSelectRange([assetId]);
  }

  function boundaryIndexAt(clientX: number): number {
    const bounds = canvasRef.current?.getBoundingClientRect();
    if (!bounds) return 0;
    return Math.max(0, Math.min(
      overview.assets.length - 1,
      Math.round((clientX - bounds.left - GUTTER - columnWidth / 2) / columnWidth),
    ));
  }

  function beginBoundaryDrag(
    event: React.PointerEvent<HTMLButtonElement>,
    phase: TripPhase,
    range: { start: number; end: number },
    edge: "start" | "end",
  ) {
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    const draft: BoundaryDrag = {
      phase,
      edge,
      startIndex: range.start,
      endIndex: range.end,
      changed: false,
    };
    boundaryDragRef.current = draft;
    setBoundaryDrag(draft);
  }

  function moveBoundaryDrag(event: React.PointerEvent<HTMLButtonElement>) {
    const current = boundaryDragRef.current;
    if (!current) return;
    const index = boundaryIndexAt(event.clientX);
    const next: BoundaryDrag = current.edge === "start"
      ? { ...current, startIndex: Math.min(index, current.endIndex), changed: true }
      : { ...current, endIndex: Math.max(index, current.startIndex), changed: true };
    boundaryDragRef.current = next;
    setBoundaryDrag(next);
  }

  function finishBoundaryDrag(event: React.PointerEvent<HTMLButtonElement>) {
    const draft = boundaryDragRef.current;
    if (!draft) return;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    suppressBoundaryClick.current = true;
    boundaryDragRef.current = null;
    setBoundaryDrag(null);
    onAdjustPhase({
      phase: draft.phase,
      startIndex: draft.startIndex,
      endIndex: draft.endIndex,
    });
  }

  function clickBoundary(phase: TripPhase, range: { start: number; end: number }) {
    if (suppressBoundaryClick.current) {
      suppressBoundaryClick.current = false;
      return;
    }
    onAdjustPhase({ phase, startIndex: range.start, endIndex: range.end });
  }

  function nudgeBoundary(
    event: React.KeyboardEvent<HTMLButtonElement>,
    phase: TripPhase,
    range: { start: number; end: number },
    edge: "start" | "end",
  ) {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const delta = event.key === "ArrowLeft" ? -1 : 1;
    const startIndex = edge === "start"
      ? Math.max(0, Math.min(range.end, range.start + delta))
      : range.start;
    const endIndex = edge === "end"
      ? Math.min(overview.assets.length - 1, Math.max(range.start, range.end + delta))
      : range.end;
    onAdjustPhase({ phase, startIndex, endIndex });
  }

  return (
    <div
      className="chronology-canvas-scroll"
      ref={scrollRef}
      onScroll={(event) => setViewport({
        left: event.currentTarget.scrollLeft,
        width: event.currentTarget.clientWidth,
      })}
    >
      <div
        className="chronology-canvas"
        ref={canvasRef}
        style={{
          width: GUTTER + Math.max(1, overview.assets.length) * columnWidth,
          "--chronology-column": `${columnWidth}px`,
          "--chronology-gutter": `${GUTTER}px`,
        } as CSSProperties}
      >
        <div className="chronology-phase-band" aria-label="Trip phases">
          <span className="chronology-sticky-label">Trip context</span>
          {overview.phases.map((phase) => {
            const solvedRange = phaseOrdinalRange(phase, overview);
            const range = boundaryDrag?.phase.id === phase.id
              ? { start: boundaryDrag.startIndex, end: boundaryDrag.endIndex }
              : solvedRange;
            if (!range || range.end < visibleWindow.start || range.start >= visibleWindow.end) return null;
            const startAsset = overview.assets[range.start];
            const endAsset = overview.assets[range.end];
            return (
              <div
                className={`chronology-phase ${phase.kind === "transit" ? "is-transit" : ""}`}
                key={phase.id}
                style={{
                  left: GUTTER + range.start * columnWidth + 3,
                  width: (range.end - range.start + 1) * columnWidth - 6,
                }}
              >
                <button
                  type="button"
                  className="phase-boundary phase-boundary-start"
                  disabled={disabled}
                  aria-label={`Move start of ${phase.label}`}
                  aria-valuetext={`Starts at clip ${range.start + 1}`}
                  onPointerDown={(event) => beginBoundaryDrag(event, phase, range, "start")}
                  onPointerMove={moveBoundaryDrag}
                  onPointerUp={finishBoundaryDrag}
                  onPointerCancel={finishBoundaryDrag}
                  onLostPointerCapture={finishBoundaryDrag}
                  onKeyDown={(event) => nudgeBoundary(event, phase, range, "start")}
                  onClick={() => clickBoundary(phase, range)}
                />
                <button
                  type="button"
                  className="chronology-phase-label"
                  disabled={disabled}
                  // The label and timezone are adjacent flex children, so the
                  // derived name runs them together ("TokyoAsia/Tokyo").
                  aria-label={`Edit ${phase.label}`}
                  onClick={() => onAdjustPhase({ phase, startIndex: range.start, endIndex: range.end })}
                >
                  <strong>{phase.label}</strong>
                  <span>{phase.kind === "transit" ? `${phase.originTimezone ?? "UTC"} → ${phase.destinationTimezone ?? "UTC"}` : phase.timezone ?? "UTC"}</span>
                </button>
                <button
                  type="button"
                  className="phase-boundary phase-boundary-end"
                  disabled={disabled}
                  aria-label={`Move end of ${phase.label}`}
                  aria-valuetext={`Ends at clip ${range.end + 1}`}
                  onPointerDown={(event) => beginBoundaryDrag(event, phase, range, "end")}
                  onPointerMove={moveBoundaryDrag}
                  onPointerUp={finishBoundaryDrag}
                  onPointerCancel={finishBoundaryDrag}
                  onLostPointerCapture={finishBoundaryDrag}
                  onKeyDown={(event) => nudgeBoundary(event, phase, range, "end")}
                  onClick={() => clickBoundary(phase, range)}
                />
                <span className="sr-only">From {startAsset?.assetId} through {endAsset?.assetId}</span>
              </div>
            );
          })}
        </div>

        <div className="chronology-ruler" aria-hidden="true">
          <span className="chronology-sticky-label">Solved order</span>
          {overview.assets.slice(visibleWindow.start, visibleWindow.end).map((asset, offset) => {
            const index = visibleWindow.start + offset;
            return (
              <span key={asset.assetId} style={{ left: GUTTER + index * columnWidth }}>
                {String(index + 1).padStart(3, "0")}
              </span>
            );
          })}
        </div>

        {lanes.map((lane) => (
          <div className="chronology-sequence-lane" key={lane.sequence.id}>
            <div className="chronology-lane-label">
              <strong>{lane.sequence.label || lane.sequence.cameraKey}</strong>
              <span>{lane.sequence.volumeHint || lane.sequence.cameraKey}</span>
            </div>
            <div className="chronology-lane-track">
              {lane.assets.filter((asset) => visibleIds.has(asset.assetId)).map((asset) => {
                const index = indexById.get(asset.assetId) ?? 0;
                const file = assetsById.get(asset.assetId);
                const moved = impact?.changes.find((change) => change.assetId === asset.assetId);
                const displayIndex = moved?.afterPosition ?? index;
                return (
                  <div key={asset.assetId}>
                    {moved && (
                      <span
                        className="chronology-ghost-block"
                        style={{ left: moved.beforePosition * columnWidth + 5 }}
                        aria-hidden="true"
                      />
                    )}
                    <button
                      type="button"
                      className={`chronology-block ${asset.assetId === focusedAssetId ? "is-focused" : ""} ${selected.has(asset.assetId) ? "is-selected" : ""} ${moved ? "will-move" : ""}`}
                      style={{ left: displayIndex * columnWidth + 5 }}
                      onClick={(event) => choose(asset.assetId, event.shiftKey)}
                      aria-pressed={selected.has(asset.assetId)}
                      disabled={disabled}
                      aria-label={`${file?.filename ?? asset.orderKey.filename}, clip ${index + 1}, ${Math.round(asset.confidence * 100)} percent confidence`}
                    >
                      <strong>{file?.filename.replace(/\.[^.]+$/, "") ?? asset.orderKey.filename}</strong>
                      <span>{asset.orderKey.filenameCounter === null ? "no counter" : `#${asset.orderKey.filenameCounter}`}</span>
                      <time dateTime={asset.displayTime?.primary ?? asset.tripTime ?? undefined}>
                        {formatChronologyTime(
                          asset.displayTime?.primary ?? asset.tripTime,
                          asset.displayTime?.timezone,
                        )}
                      </time>
                    </button>
                  </div>
                );
              })}
              {selectedIds.length > 0 && lane.assets.some((asset) => selected.has(asset.assetId)) && (
                <div className="chronology-placement-targets" aria-label="Place selected run">
                  {overview.assets.slice(visibleWindow.start, visibleWindow.end).filter((asset) => !selected.has(asset.assetId)).map((asset) => {
                    const index = indexById.get(asset.assetId) ?? 0;
                    return (
                      <span key={asset.assetId} style={{ left: index * columnWidth }}>
                        <button type="button" disabled={disabled} onClick={() => onPlaceSelection(asset.assetId, "before")} aria-label={`Place selected run before ${asset.orderKey.filename}`} />
                        <button type="button" disabled={disabled} onClick={() => onPlaceSelection(asset.assetId, "after")} aria-label={`Place selected run after ${asset.orderKey.filename}`} />
                      </span>
                    );
                  })}
                </div>
              )}
            </div>
          </div>
        ))}

        {impactExtent && (
          <div
            className="chronology-impact-extent"
            style={{
              left: GUTTER + impactExtent.start * columnWidth,
              width: (impactExtent.end - impactExtent.start + 1) * columnWidth,
            }}
            aria-hidden="true"
          />
        )}
      </div>
    </div>
  );
}
