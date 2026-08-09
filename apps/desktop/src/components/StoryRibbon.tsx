import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { CSSProperties, RefObject } from "react";
import type { Asset } from "../types";
import {
  RIBBON_ZOOM_MAX,
  RIBBON_ZOOM_MIN,
  RIBBON_ZOOM_STEP,
  ribbonHeight,
  ribbonLaneRows,
} from "../review/layout";
import { formatDate } from "../review/format";

/** A contiguous run of clips sharing the same trip context. */
export interface ContextRun {
  start: number;
  end: number;
  asset: Asset;
  /** Present for persisted trip phases; absent for browser-demo context. */
  phaseId?: string | null;
}

export type ContextDirection = "through" | "from";

interface ContextAnchor {
  index: number;
  x: number;
  top: number;
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
  /** Collapsed to a 28px strip. */
  collapsed: boolean;
  /** One merged lane instead of one per camera — the Focus overlay variant. */
  merged: boolean;
  /** Dragged override; null sizes the ribbon to its own content. */
  height: number | null;
  scrollRef: RefObject<HTMLDivElement | null>;
  onZoomChange(next: number): void;
  onCenter(): void;
  onSelect(id: string): void;
  onToggleCollapsed(): void;
  onRequestContext(assetId: string, direction: ContextDirection): void;
  onEditContext(phaseId: string, start: number, end: number): void;
  /** Corrects the camera identification for every clip in a lane. */
  onRenameCamera(from: string, to: string): void;
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
  collapsed,
  merged,
  height: heightOverride,
  scrollRef,
  onZoomChange,
  onCenter,
  onSelect,
  onToggleCollapsed,
  onRequestContext,
  onEditContext,
  onRenameCamera,
}: StoryRibbonProps) {
  // The lane whose name is being corrected, and the draft. Only one at a time:
  // the gutter is 96px, so a second open field would have nowhere to go.
  const [editingLane, setEditingLane] = useState<string | null>(null);
  const [laneDraft, setLaneDraft] = useState("");
  const [hoveredContextIndex, setHoveredContextIndex] = useState<number | null>(null);
  const [contextAnchor, setContextAnchor] = useState<ContextAnchor | null>(null);
  const contextPopoverRef = useRef<HTMLDivElement>(null);

  // Enter and blur both commit, so a rename survives clicking away. An empty or
  // unchanged draft closes the field without a write.
  function commitLaneRename(camera: string) {
    const next = laneDraft.trim();
    setEditingLane(null);
    if (next && next !== camera) onRenameCamera(camera, next);
  }
  // Merged draws one lane holding every clip; otherwise one lane per camera.
  const laneRows: Array<[string, Asset[]]> = merged ? [["", assets]] : lanes;
  // A dragged height wins over the content height, and the lane stack scrolls
  // to fit inside it. Collapsed always wins over both.
  const height = collapsed
    ? ribbonHeight(laneRows.length, true)
    : heightOverride ?? ribbonHeight(laneRows.length, false);
  const contextAnchorAsset = contextAnchor === null ? null : assets[contextAnchor.index] ?? null;

  useEffect(() => {
    if (!contextAnchor) return;
    contextPopoverRef.current?.querySelector<HTMLButtonElement>(".context-direction-button")?.focus();

    function dismissOnPointerDown(event: PointerEvent) {
      if (!contextPopoverRef.current?.contains(event.target as Node)) setContextAnchor(null);
    }
    function dismissOnEscape(event: KeyboardEvent) {
      if (event.key === "Escape") setContextAnchor(null);
    }

    document.addEventListener("pointerdown", dismissOnPointerDown);
    document.addEventListener("keydown", dismissOnEscape);
    return () => {
      document.removeEventListener("pointerdown", dismissOnPointerDown);
      document.removeEventListener("keydown", dismissOnEscape);
    };
  }, [contextAnchor]);

  function contextIndexAt(clientX: number, track: HTMLDivElement) {
    const index = Math.floor((clientX - track.getBoundingClientRect().left) / zoom);
    if (index < 0 || index >= assets.length) return null;
    return contextRuns.some((run) => index >= run.start && index <= run.end) ? null : index;
  }

  function chooseContext(direction: ContextDirection) {
    if (!contextAnchorAsset) return;
    onRequestContext(contextAnchorAsset.id, direction);
    setContextAnchor(null);
    setHoveredContextIndex(null);
  }

  return (
    <div className="chronology" style={{ height }}>
      <div className="chronology-heading">
        {/* The old heading spent 45px on an eyebrow plus a line restating the
            clip count the queue badge already carries. */}
        <button className="ribbon-collapse" onClick={onToggleCollapsed} aria-expanded={!collapsed} aria-keyshortcuts="T">
          {collapsed ? "▸" : "▾"} Story ribbon <kbd>T</kbd>
        </button>
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
      {collapsed ? null : laneRows.length > 0 ? (
        <div className="story-map">
          <div className="story-labels" style={{ gridTemplateRows: `20px 14px ${ribbonLaneRows(laneRows.length)}` }}>
            <span className="context-label" aria-hidden="true">Context</span>
            <span className="order-label" aria-hidden="true">Order</span>
            {/* The gutter is interactive, so it cannot be aria-hidden wholesale
                the way the tracks are. */}
            {merged ? null : lanes.map(([camera]) => (
              <span className="lane-name" key={camera}>
                {editingLane === camera ? (
                  <input
                    className="lane-name-field"
                    autoFocus
                    value={laneDraft}
                    aria-label={`Rename camera ${camera}`}
                    onChange={(event) => setLaneDraft(event.target.value)}
                    onBlur={() => {
                      commitLaneRename(camera);
                    }}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") commitLaneRename(camera);
                      else if (event.key === "Escape") setEditingLane(null);
                    }}
                  />
                ) : (
                  <button
                    className="lane-name-button"
                    title={`${camera} — click to rename`}
                    onClick={() => {
                      setLaneDraft(camera);
                      setEditingLane(camera);
                    }}
                  >{camera}</button>
                )}
              </span>
            ))}
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
                gridTemplateRows: `20px 14px ${ribbonLaneRows(laneRows.length)}`,
                "--clip-step": `${zoom}px`,
              } as CSSProperties}
            >
              <div
                className="context-track"
                onPointerMove={(event) => {
                  if (contextAnchor !== null) return;
                  setHoveredContextIndex(contextIndexAt(event.clientX, event.currentTarget));
                }}
                onPointerLeave={() => {
                  if (contextAnchor === null) setHoveredContextIndex(null);
                }}
              >
                {contextRuns.map((run) => {
                  const primary = run.asset.phase || run.asset.location || "Context not tagged";
                  const detail = [
                    run.asset.location && run.asset.location !== primary ? run.asset.location : null,
                    run.asset.timezone,
                    run.asset.storyBeat,
                  ].filter(Boolean).join(" · ") || "Add phase, place, timezone or story beat";
                  return (
                    <button
                      type="button"
                      className="context-segment"
                      key={`${run.start}-${assetContextKey(run.asset)}`}
                      style={{ left: run.start * zoom + 3, width: (run.end - run.start + 1) * zoom - 6 }}
                      title={`${primary} · ${detail}`}
                      onClick={() => {
                        if (run.phaseId) onEditContext(run.phaseId, run.start, run.end);
                      }}
                      aria-label={run.phaseId ? `Edit ${primary} context` : `${primary} context`}
                    >
                      <strong>{primary}</strong>
                      <span aria-hidden="true"> · {detail}</span>
                    </button>
                  );
                })}
                {(hoveredContextIndex !== null || contextAnchor !== null) && (
                  <button
                    type="button"
                    className={`context-ghost ${contextAnchor !== null ? "is-anchored" : ""}`}
                    style={{ left: (contextAnchor?.index ?? hoveredContextIndex ?? 0) * zoom + 3, width: Math.max(18, zoom - 6) }}
                    aria-label={`Add context at clip ${(contextAnchor?.index ?? hoveredContextIndex ?? 0) + 1}`}
                    onPointerMove={(event) => event.stopPropagation()}
                    onClick={(event) => {
                      const index = contextAnchor?.index ?? hoveredContextIndex;
                      if (index === null) return;
                      const rect = event.currentTarget.getBoundingClientRect();
                      const clickX = event.clientX || rect.left + rect.width / 2;
                      setContextAnchor({
                        index,
                        x: Math.min(window.innerWidth - 158, Math.max(158, clickX)),
                        top: rect.top,
                      });
                    }}
                  >
                    <span aria-hidden="true">+</span>
                  </button>
                )}
              </div>
              <div className="order-track" aria-hidden="true">
                {assets.map((asset, index) => (
                  <span className={asset.id === selectedId ? "active" : ""} style={{ left: index * zoom }} key={asset.id}>
                    {String(index + 1).padStart(3, "0")}
                  </span>
                ))}
              </div>
              {laneRows.map(([camera, cameraAssets]) => (
                <div className="lane-track" key={camera || "merged"}>
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
      {contextAnchor && typeof document !== "undefined" && createPortal(
        <div
          ref={contextPopoverRef}
          className="context-anchor-popover"
          role="group"
          aria-label={`Add context at clip ${contextAnchor.index + 1}`}
          style={{ left: contextAnchor.x, top: Math.max(8, contextAnchor.top - 8) }}
        >
          <div className="context-popover-heading">
            <span>Clip #{contextAnchor.index + 1}</span>
            <strong>Set context boundary</strong>
            <button type="button" aria-label="Cancel adding context" onClick={() => setContextAnchor(null)}>×</button>
          </div>
          <div className="context-popover-actions">
            <button type="button" className="context-direction-button" onClick={() => chooseContext("through")}><span aria-hidden="true">←</span> Add through here</button>
            <button type="button" className="context-direction-button" onClick={() => chooseContext("from")}>Add from here <span aria-hidden="true">→</span></button>
          </div>
        </div>,
        document.body,
      )}
    </div>
  );
}
