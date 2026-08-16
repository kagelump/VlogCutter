import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { CSSProperties } from "react";
import type { Asset } from "../types";
import { formatDuration } from "../review/format";

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
  onRotate(addition: 90 | 180): void;
  /** An absolute orientation, chosen rather than cycled to. */
  onSetRotation(degrees: number): void;
  onResetRotation(): void;
  onEnterFocus(): void;
}

/** The four cardinal orientations, as absolute choices. */
const ORIENTATIONS = [0, 90, 180, 270] as const;

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
  onSetRotation,
  onResetRotation,
  onEnterFocus,
}: TransportBarProps) {
  const [orientationOpen, setOrientationOpen] = useState(false);
  const orientationButtonRef = useRef<HTMLButtonElement>(null);
  const orientationMenuRef = useRef<HTMLDivElement>(null);
  const max = Math.max(0, durationSeconds);
  const progress = max > 0 ? Math.min(100, (positionSeconds / max) * 100) : 0;
  const at = (ms: number) => (max > 0 ? Math.min(100, (ms / 1000 / max) * 100) : 0);

  useEffect(() => {
    if (!orientationOpen) return;
    orientationMenuRef.current?.querySelector<HTMLButtonElement>("button")?.focus();

    function dismissOnPointerDown(event: PointerEvent) {
      const target = event.target as Node;
      if (!orientationMenuRef.current?.contains(target) && !orientationButtonRef.current?.contains(target)) {
        setOrientationOpen(false);
      }
    }
    function dismissOnEscape(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setOrientationOpen(false);
        orientationButtonRef.current?.focus();
      }
    }

    document.addEventListener("pointerdown", dismissOnPointerDown);
    document.addEventListener("keydown", dismissOnEscape);
    return () => {
      document.removeEventListener("pointerdown", dismissOnPointerDown);
      document.removeEventListener("keydown", dismissOnEscape);
    };
  }, [orientationOpen]);

  function chooseRotation(action: () => void) {
    action();
    setOrientationOpen(false);
  }

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
        ref={orientationButtonRef}
        className={`transport-toggle ${rotationOverridden ? "on" : ""}`}
        onClick={() => setOrientationOpen((open) => !open)}
        aria-label="Frame orientation"
        aria-haspopup="menu"
        aria-expanded={orientationOpen}
        title={rotationOverridden
          ? `Orientation set to ${asset.rotationDegrees}°, overriding automatic ${asset.defaultRotationDegrees}° · O rotates · ⇧O flips`
          : `Automatic orientation (${asset.defaultRotationDegrees}°) · O rotates · ⇧O flips`}
      >⟳</button>
      {orientationOpen && typeof document !== "undefined" && createPortal(
        <div
          ref={orientationMenuRef}
          className="orientation-menu"
          role="menu"
          aria-label="Frame orientation"
          style={{
            left: Math.min(window.innerWidth - 8, orientationButtonRef.current?.getBoundingClientRect().right ?? window.innerWidth - 8),
            top: Math.max(8, (orientationButtonRef.current?.getBoundingClientRect().top ?? 8) - 6),
          }}
        >
          <div className="orientation-menu-heading">
            <strong>Frame orientation</strong>
            <span>{rotationOverridden ? `Overriding automatic ${asset.defaultRotationDegrees}°` : "Following camera metadata"}</span>
          </div>
          {/* Absolute choices, not another way to cycle. Someone correcting a
              wrong guess knows which way up the clip is; making them count
              clicks to get there is the thing this replaces. */}
          <button
            type="button"
            role="menuitemradio"
            aria-checked={!rotationOverridden}
            onClick={() => chooseRotation(onResetRotation)}
          >
            <span>Auto ({asset.defaultRotationDegrees}°)</span>
            <small>Use camera metadata</small>
          </button>
          {ORIENTATIONS.map((degrees) => (
            <button
              key={degrees}
              type="button"
              role="menuitemradio"
              // An explicit angle that equals the automatic one is
              // indistinguishable from Auto, because that is exactly what the
              // catalog stores: an override *is* a rotation differing from the
              // probed default.
              aria-checked={rotationOverridden && asset.rotationDegrees === degrees}
              onClick={() => chooseRotation(() => onSetRotation(degrees))}
            >
              <span>{degrees}°</span>
              {degrees === asset.defaultRotationDegrees && <small>same as automatic</small>}
            </button>
          ))}
          <p className="orientation-menu-keys">
            <kbd>O</kbd> rotates 90° · <kbd>⇧O</kbd> flips 180°
          </p>
        </div>,
        document.body,
      )}
      <span className={`source-badge ${asset.proxyAvailable ? "" : "original"}`}>
        {asset.proxyAvailable ? "PROXY" : "ORIGINAL"}
      </span>
      <button className="transport-toggle" onClick={onEnterFocus} aria-label="Fullscreen" aria-keyshortcuts="F">⤢</button>
    </div>
  );
}
