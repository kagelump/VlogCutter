import type { CSSProperties, RefObject } from "react";
import type { Asset } from "../types";
import {
  RIBBON_ZOOM_MAX,
  RIBBON_ZOOM_MIN,
  RIBBON_ZOOM_STEP,
} from "../review/layout";

/** A contiguous run of clips sharing the same trip context. */
export interface ContextRun {
  start: number;
  end: number;
  asset: Asset;
}

/**
 * The identity of a clip's trip context. Two adjacent clips with the same key
 * belong to the same context segment.
 */
export function assetContextKey(asset: Asset) {
  return [asset.phase, asset.location, asset.timezone, asset.storyBeat]
    .map((value) => value?.trim() || "")
    .join("\u0000");
}

export interface StoryRibbonProps {
  /** Non-removed clips, in solved relative order. */
  assets: Asset[];
  lanes: Array<[string, Asset[]]>;
  contextRuns: ContextRun[];
  selected: Asset | null;
  selectedId: string;
  zoom: number;
  scrollRef: RefObject<HTMLDivElement | null>;
  formatDate(value: string | null): string;
  onZoomChange(next: number): void;
  onCenter(): void;
  onSelect(id: string): void;
}

/**
 * The relative-order ribbon (spec §3.8).
 *
 * Five cameras with disagreeing clocks make a flat queue useless for the
 * question "where am I in the trip" — a lane-crossing gap is invisible in a
 * list. This is the only surface that answers it.
 */
export function StoryRibbon({
  assets,
  lanes,
  contextRuns,
  selected,
  selectedId,
  zoom,
  scrollRef,
  formatDate,
  onZoomChange,
  onCenter,
  onSelect,
}: StoryRibbonProps) {
  return (
    <div className="chronology">
      <div className="chronology-heading">
        <div>
          <span className="eyebrow">Story ribbon</span>
          <strong>{assets.length ? `${assets.length} clips in relative order` : "Relative order across sources"}</strong>
        </div>
        <div className="chronology-actions">
          <span className="chronology-position">
            {selected && !selected.removed
              ? `#${assets.findIndex((asset) => asset.id === selected.id) + 1} · ${formatDate(selected.bestTime)}`
              : "No active clip selected"}
          </span>
          <div className="zoom-control" aria-label="Story ribbon zoom">
            <button
              onClick={() => onZoomChange(Math.max(RIBBON_ZOOM_MIN, zoom - RIBBON_ZOOM_STEP))}
              disabled={zoom === RIBBON_ZOOM_MIN}
              aria-label="Zoom story ribbon out"
              aria-keyshortcuts="BracketLeft"
            >−<kbd>[</kbd></button>
            <input
              type="range"
              min={RIBBON_ZOOM_MIN}
              max={RIBBON_ZOOM_MAX}
              step={RIBBON_ZOOM_STEP}
              value={zoom}
              onChange={(event) => onZoomChange(Number(event.target.value))}
              aria-label="Story ribbon zoom level"
            />
            <button
              onClick={() => onZoomChange(Math.min(RIBBON_ZOOM_MAX, zoom + RIBBON_ZOOM_STEP))}
              disabled={zoom === RIBBON_ZOOM_MAX}
              aria-label="Zoom story ribbon in"
              aria-keyshortcuts="BracketRight"
            >+<kbd>]</kbd></button>
          </div>
          <button className="center-story-button" onClick={onCenter} aria-keyshortcuts="C">
            Center <kbd>C</kbd>
          </button>
        </div>
      </div>
      {lanes.length > 0 ? (
        <div className="story-map">
          <div className="story-labels" aria-hidden="true">
            <span className="context-label">Trip context</span>
            <span className="order-label">Order</span>
            {lanes.map(([camera]) => <span className="lane-name" title={camera} key={camera}>{camera}</span>)}
          </div>
          <div
            className="story-scroll"
            ref={scrollRef}
            onWheel={(event) => {
              if (Math.abs(event.deltaY) <= Math.abs(event.deltaX) || event.ctrlKey) return;
              event.preventDefault();
              event.currentTarget.scrollLeft += event.deltaY;
            }}
          >
            <div
              className="story-canvas"
              style={{
                width: `${Math.max(zoom, assets.length * zoom)}px`,
                minWidth: "100%",
                "--clip-step": `${zoom}px`,
              } as CSSProperties}
            >
              <div className="context-track">
                {contextRuns.map((run) => {
                  const primary = run.asset.phase || run.asset.location || "Context not tagged";
                  const detail = [
                    run.asset.location && run.asset.location !== primary ? run.asset.location : null,
                    run.asset.timezone,
                    run.asset.storyBeat,
                  ].filter(Boolean).join(" · ") || "Add phase, place, timezone or story beat";
                  return (
                    <div
                      className="context-segment"
                      key={`${run.start}-${assetContextKey(run.asset)}`}
                      style={{ left: run.start * zoom + 3, width: (run.end - run.start + 1) * zoom - 6 }}
                      title={`${primary} · ${detail}`}
                    >
                      <strong>{primary}</strong>
                      <span>{detail}</span>
                    </div>
                  );
                })}
              </div>
              <div className="order-track" aria-hidden="true">
                {assets.map((asset, index) => (
                  <span className={asset.id === selectedId ? "active" : ""} style={{ left: index * zoom }} key={asset.id}>
                    {String(index + 1).padStart(3, "0")}
                  </span>
                ))}
              </div>
              {lanes.map(([camera, cameraAssets]) => (
                <div className="lane-track" key={camera}>
                  {cameraAssets.map((asset) => {
                    const index = assets.findIndex((candidate) => candidate.id === asset.id);
                    return (
                      <button
                        key={asset.id}
                        className={`timeline-clip ${asset.offsetSeconds ? "corrected" : ""} ${asset.id === selectedId ? "active" : ""}`}
                        style={{ left: index * zoom + 4, width: zoom - 8 }}
                        onClick={() => onSelect(asset.id)}
                        aria-label={`Clip ${index + 1}, ${asset.filename}, ${camera}`}
                        title={`#${index + 1} · ${asset.filename} · ${formatDate(asset.bestTime)}`}
                      >
                        <span>{asset.filename.replace(/\.[^.]+$/, "")}</span>
                        {asset.labels.length > 0 && <i aria-hidden="true" />}
                      </button>
                    );
                  })}
                </div>
              ))}
            </div>
          </div>
        </div>
      ) : <div className="empty-lanes">Camera lanes appear after ingest.</div>}
    </div>
  );
}
