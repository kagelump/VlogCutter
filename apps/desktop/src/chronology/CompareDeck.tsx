/**
 * The compare deck: the chronology desk's answer to "does this clip belong
 * here?".
 *
 * The desk used to be entirely typographic — filenames and timestamps — which
 * is the one form of evidence that is useless when a camera clock is the thing
 * under suspicion. The deck puts the frame back: the focused clip plays, and
 * the clips the solver placed either side of it sit next to it as stills.
 *
 * "Either side" means either side in *solved order*, across cards. A clip's
 * lane neighbours are its filename neighbours, which the solver already agrees
 * about; the interesting question is always about the interleave between cards,
 * and that is what this deck shows.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import { buildMediaBuffer } from "../review/mediaBuffer";
import { isRawOrientationThumbnail, renderedRotation } from "../review/rotation";
import { VideoStage } from "../components/VideoStage";
import type { Asset, MediaOrigin } from "../types";
import type { AssetChronology, ChronologyOverview } from "../chronologyTypes";
import { formatChronologyTime } from "./geometry";
import type { CSSProperties } from "react";

interface Props {
  overview: ChronologyOverview;
  assetsById: Map<string, Asset>;
  focusedAssetId: string | null;
  mediaOrigin: MediaOrigin | null;
  collapsed: boolean;
  /** True while a correction preview owns the desk; the deck stops competing. */
  disabled?: boolean;
  onFocusAsset(assetId: string): void;
  onToggleCollapsed(): void;
}

function clock(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  const whole = Math.floor(seconds);
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, "0")}`;
}

function placedTime(entry: AssetChronology | undefined): string {
  if (!entry) return "";
  return formatChronologyTime(
    entry.displayTime?.primary ?? entry.tripTime,
    entry.displayTime?.timezone,
  );
}

/** One of the two flanking stills. */
function Neighbour({
  side,
  entry,
  asset,
  disabled,
  onFocus,
}: {
  side: "previous" | "next";
  entry: AssetChronology;
  asset: Asset | undefined;
  disabled: boolean;
  onFocus(): void;
}) {
  const filename = asset?.filename ?? entry.orderKey.filename;
  const time = placedTime(entry);
  const thumbnailUrl = asset ? api.thumbnailUrl(asset) : null;
  const rotation = asset ? renderedRotation(asset, isRawOrientationThumbnail(asset)) : 0;
  return (
    <button
      type="button"
      className={`chronology-deck-neighbour is-${side}`}
      disabled={disabled}
      aria-label={`${side === "previous" ? "Previous" : "Next"} in order: ${filename}, ${time}`}
      onClick={onFocus}
    >
      <span className="chronology-deck-neighbour-label">
        {side === "previous" ? "◀ Previous in order" : "Next in order ▶"}
      </span>
      <span
        className={`chronology-deck-still ${rotation % 180 !== 0 ? "quarter-turn" : ""}`}
        style={{ "--thumbnail-rotation": `${rotation}deg` } as CSSProperties}
      >
        {thumbnailUrl
          ? <img src={thumbnailUrl} alt="" loading="lazy" />
          : <span className="thumbnail-empty">No frame</span>}
      </span>
      <strong>{filename}</strong>
      <em>{time}</em>
    </button>
  );
}

export function CompareDeck({
  overview,
  assetsById,
  focusedAssetId,
  mediaOrigin,
  collapsed,
  disabled = false,
  onFocusAsset,
  onToggleCollapsed,
}: Props) {
  const stageRef = useRef<HTMLDivElement>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const [playing, setPlaying] = useState(false);
  const [buffering, setBuffering] = useState(false);
  const [muted, setMuted] = useState(true);
  const [position, setPosition] = useState(0);
  const [duration, setDuration] = useState(0);

  const index = focusedAssetId
    ? overview.assets.findIndex((entry) => entry.assetId === focusedAssetId)
    : -1;
  const previous = index > 0 ? overview.assets[index - 1] : undefined;
  const next = index >= 0 ? overview.assets[index + 1] : undefined;
  const focusedEntry = index >= 0 ? overview.assets[index] : undefined;
  const focusedAsset = focusedAssetId ? assetsById.get(focusedAssetId) ?? null : null;

  /**
   * The ±1 window, ordered by the solver rather than by the review queue. It is
   * rebuilt when the origin changes because every URL carries the origin's
   * session token.
   */
  const media = useMemo(() => {
    const ordered = [previous, focusedEntry, next]
      .map((entry) => (entry ? assetsById.get(entry.assetId) : undefined))
      .filter((asset): asset is Asset => Boolean(asset));
    return buildMediaBuffer(ordered, focusedAssetId, mediaOrigin, api.thumbnailUrl);
  }, [assetsById, focusedAssetId, focusedEntry, mediaOrigin, next, previous]);
  const mediaUrl = media.find((entry) => entry.active)?.url ?? null;

  // A new clip in the frame starts from its own beginning, paused.
  useEffect(() => {
    setPosition(0);
    setDuration(focusedAsset?.durationSeconds ?? 0);
    setPlaying(false);
  }, [focusedAsset?.durationSeconds, focusedAssetId]);

  useEffect(() => {
    if (collapsed || disabled) return;
    function step(event: KeyboardEvent) {
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
      // A phase boundary being nudged has already claimed the arrow, and a
      // caret in the clock form needs it more than the deck does.
      if (event.defaultPrevented) return;
      const target = event.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA"
        || target.tagName === "SELECT" || target.isContentEditable)) return;
      const destination = event.key === "ArrowLeft" ? previous : next;
      if (!destination) return;
      event.preventDefault();
      onFocusAsset(destination.assetId);
    }
    window.addEventListener("keydown", step);
    return () => window.removeEventListener("keydown", step);
  }, [collapsed, disabled, next, onFocusAsset, previous]);

  function togglePlay() {
    const element = videoRef.current;
    if (!element) return;
    if (element.paused) void element.play().catch(() => setPlaying(false));
    else element.pause();
  }

  function seekTo(seconds: number) {
    const element = videoRef.current;
    if (element) element.currentTime = seconds;
    setPosition(seconds);
  }

  return (
    <section className={`chronology-deck ${collapsed ? "is-collapsed" : ""}`} aria-label="Compare deck">
      <header className="chronology-deck-header">
        <button
          type="button"
          className="chronology-deck-toggle"
          aria-expanded={!collapsed}
          aria-label={collapsed ? "Show the compare deck" : "Hide the compare deck"}
          onClick={onToggleCollapsed}
        >
          <span aria-hidden="true">{collapsed ? "▸" : "▾"}</span>
          Compare
        </button>
        {index >= 0 && <span className="chronology-deck-position">Clip {index + 1} of {overview.assets.length}</span>}
        {!collapsed && focusedAsset && (
          <span className="chronology-deck-hint">← → steps along the solved order</span>
        )}
      </header>

      {!collapsed && (
        focusedAsset ? (
          <div className="chronology-deck-body">
            {previous
              ? <Neighbour
                  side="previous"
                  entry={previous}
                  asset={assetsById.get(previous.assetId)}
                  disabled={disabled}
                  onFocus={() => onFocusAsset(previous.assetId)}
                />
              : <div className="chronology-deck-neighbour is-empty" aria-hidden="true"><span>Start of the trip</span></div>}

            <div className="chronology-deck-focus" role="group" aria-label={`Focused clip ${focusedAsset.filename}`}>
              <VideoStage
                selected={focusedAsset}
                media={media}
                mediaUrl={mediaUrl}
                playing={playing}
                buffering={buffering}
                muted={muted}
                autoplay={false}
                stageRef={stageRef}
                videoRef={videoRef}
                onTogglePlay={togglePlay}
                onBufferingChange={setBuffering}
                onPlayingChange={setPlaying}
                onDurationChange={setDuration}
                onPositionChange={setPosition}
              />
              <div className="chronology-deck-transport">
                <button
                  type="button"
                  className="chronology-deck-play"
                  disabled={!mediaUrl}
                  aria-label={playing ? "Pause" : "Play"}
                  onClick={togglePlay}
                >
                  <span aria-hidden="true">{playing ? "❚❚" : "▶"}</span>
                </button>
                <input
                  type="range"
                  min={0}
                  max={Math.max(duration, 0.1)}
                  step={0.05}
                  value={Math.min(position, duration || 0)}
                  disabled={!mediaUrl}
                  aria-label={`Scrub ${focusedAsset.filename}`}
                  onChange={(event) => seekTo(Number(event.target.value))}
                />
                <span className="chronology-deck-elapsed">{clock(position)} / {clock(duration)}</span>
                <button
                  type="button"
                  aria-label={muted ? "Unmute" : "Mute"}
                  aria-pressed={muted}
                  onClick={() => setMuted((current) => !current)}
                >
                  <span aria-hidden="true">{muted ? "🔇" : "🔊"}</span>
                </button>
              </div>
              <div className="chronology-deck-caption">
                <strong>{focusedAsset.filename}</strong>
                <em>{placedTime(focusedEntry)}</em>
              </div>
            </div>

            {next
              ? <Neighbour
                  side="next"
                  entry={next}
                  asset={assetsById.get(next.assetId)}
                  disabled={disabled}
                  onFocus={() => onFocusAsset(next.assetId)}
                />
              : <div className="chronology-deck-neighbour is-empty" aria-hidden="true"><span>End of the trip</span></div>}
          </div>
        ) : (
          <div className="chronology-deck-body is-empty">
            <p>Choose a clip on the canvas to see it here.</p>
          </div>
        )
      )}
    </section>
  );
}
