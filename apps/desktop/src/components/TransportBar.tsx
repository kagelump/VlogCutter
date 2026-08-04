import type { CSSProperties } from "react";
import type { Asset } from "../types";

export interface TransportBarProps {
  asset: Asset;
  playing: boolean;
  muted: boolean;
  positionSeconds: number;
  durationSeconds: number;
  bestMomentMs: number | null;
  segmentStartsMs: number[];
  rotationOverridden: boolean;
  onTogglePlay(): void;
  onSeek(seconds: number): void;
  onSeekBy(delta: number): void;
  onToggleMute(): void;
  onStep(delta: number): void;
  onRotate(): void;
  onEnterFocus(): void;
}

function formatDuration(seconds: number) {
  const safe = Number.isFinite(seconds) ? Math.max(0, seconds) : 0;
  return `${Math.floor(safe / 60)}:${Math.floor(safe % 60).toString().padStart(2, "0")}`;
}

/**
 * The single transport (spec §3.5).
 *
 * This replaces both the 46px bar that duplicated play, timecode and mute,
 * and the overlay that hid itself during playback. A scrubber that vanishes
 * while you are watching is unaimable, and it covered the frame besides — so
 * the bar is permanent and never reacts to hover or playback state.
 *
 * The best moment is a marker here rather than a button printing a timestamp
 * as text: as a number it is inert, as a marker it is a control.
 */
export function TransportBar({
  asset,
  playing,
  muted,
  positionSeconds,
  durationSeconds,
  bestMomentMs,
  segmentStartsMs,
  rotationOverridden,
  onTogglePlay,
  onSeek,
  onSeekBy,
  onToggleMute,
  onStep,
  onRotate,
  onEnterFocus,
}: TransportBarProps) {
  const max = Math.max(0, durationSeconds);
  const progress = max > 0 ? Math.min(100, (positionSeconds / max) * 100) : 0;
  const at = (ms: number) => (max > 0 ? Math.min(100, (ms / 1000 / max) * 100) : 0);

  return (
    <div className="transport">
      <button className="transport-step" onClick={() => onStep(-1)} aria-label="Previous clip" aria-keyshortcuts="K">‹</button>
      <button className="transport-play" onClick={onTogglePlay} aria-label={playing ? "Pause" : "Play"} aria-keyshortcuts="Space">
        {playing ? "❚❚" : "▶"}
      </button>
      <button className="transport-step" onClick={() => onStep(1)} aria-label="Next clip" aria-keyshortcuts="J">›</button>
      <button className="transport-skip" onClick={() => onSeekBy(-15)} aria-label="Back 15 seconds">−15</button>
      <button className="transport-skip" onClick={() => onSeekBy(15)} aria-label="Forward 15 seconds">+15</button>

      <time className="transport-time">{formatDuration(positionSeconds)} / {formatDuration(max)}</time>

      <div className="scrubber">
        {segmentStartsMs.map((ms) => (
          <span className="segment-tick" key={ms} style={{ left: `${at(ms)}%` }} aria-hidden="true" />
        ))}
        {bestMomentMs !== null && (
          <button
            className="best-moment-marker"
            style={{ left: `${at(bestMomentMs)}%` }}
            onClick={() => onSeek(bestMomentMs / 1000)}
            aria-label="Jump to the best moment"
            aria-keyshortcuts="B"
            title="Best moment · B"
          />
        )}
        <input
          type="range"
          min="0"
          max={max || 0}
          step="0.01"
          value={Math.min(positionSeconds, max)}
          onChange={(event) => onSeek(Number(event.currentTarget.value))}
          aria-label="Video position"
          style={{ "--seek-progress": `${progress}%` } as CSSProperties}
        />
      </div>

      <button className={`transport-toggle ${muted ? "on" : ""}`} onClick={onToggleMute} aria-pressed={muted} aria-label={muted ? "Unmute" : "Mute"}>
        {muted ? "Muted" : "Sound"}
      </button>
      {/* One glyph, and a popover behind it. The illustrated two-card control
          this replaces cost 107px of permanent rail for a rare action. */}
      <button
        className={`transport-toggle ${rotationOverridden ? "on" : ""}`}
        onClick={onRotate}
        aria-label="Frame orientation"
        title={rotationOverridden ? `Rotation overridden — ${asset.rotationDegrees}°` : "Landscape (automatic)"}
      >⟳</button>
      <span className={`source-badge ${asset.proxyAvailable ? "" : "original"}`}>
        {asset.proxyAvailable ? "PROXY" : "ORIGINAL"}
      </span>
      <button className="transport-toggle" onClick={onEnterFocus} aria-label="Fullscreen" aria-keyshortcuts="F">⤢</button>
    </div>
  );
}
