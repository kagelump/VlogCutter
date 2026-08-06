import type { CSSProperties, RefObject } from "react";
import type { Asset } from "../types";
import { formatDate } from "../review/format";
import { isRawOrientationThumbnail } from "../review/rotation";

/**
 * One clip mounted in the stage: the selected one and its two neighbours.
 *
 * The neighbours exist so that J and K do not pay a load. The shell builds
 * these because only it knows the queue order; the stage only draws them.
 */
export interface BufferedMedia {
  asset: Asset;
  /** -1, 0, or 1 relative to the selection. */
  offset: number;
  active: boolean;
  url: string | null;
  /**
   * Whether the media behind `url` leaves the pixels raw and carries rotation
   * as data. Resolved by the shell, because it depends on which of the proxy
   * and the original the URL points at.
   */
  mediaIsRawOriented: boolean;
  /** Degrees to turn the frame, already resolved against the representation. */
  rotation: number;
  displayAspect: number;
  posterUrl: string | null;
}

export interface VideoStageProps {
  selected: Asset | null;
  media: BufferedMedia[];
  /** Null when the selected clip has no playable URL — the slate states why. */
  mediaUrl: string | null;
  playing: boolean;
  buffering: boolean;
  muted: boolean;
  autoplay: boolean;
  /** Desktop, with the original in reach. */
  canMakeProxy: boolean;
  busy: boolean;
  stageRef: RefObject<HTMLDivElement | null>;
  /** Attached to the active element only; the shell drives playback through it. */
  videoRef: RefObject<HTMLVideoElement | null>;
  onTogglePlay(): void;
  onBufferingChange(buffering: boolean): void;
  onPlayingChange(playing: boolean): void;
  onDurationChange(seconds: number): void;
  onPositionChange(seconds: number): void;
  onMakeProxy(): void;
}

/**
 * The frame (spec §3.4).
 *
 * Three clips are mounted and only one is shown. That is what makes stepping
 * through the queue feel like scrubbing one long take rather than opening
 * files, and it is why every handler here is guarded on `entry.active` — an
 * inactive element still fires loadedmetadata and would otherwise overwrite
 * the transport's idea of where playback is.
 */
export function VideoStage({
  selected,
  media,
  mediaUrl,
  playing,
  buffering,
  muted,
  autoplay,
  canMakeProxy,
  busy,
  stageRef,
  videoRef,
  onTogglePlay,
  onBufferingChange,
  onPlayingChange,
  onDurationChange,
  onPositionChange,
  onMakeProxy,
}: VideoStageProps) {
  return (
    <div ref={stageRef} className={`viewer-stage ${playing ? "is-playing" : "is-paused"}`}>
      {selected ? (
        !selected.orientationProbed && selected.originalAvailable ? (
          <div className="offline-slate">
            <strong>Reading frame orientation…</strong>
            <span>{selected.filename}</span>
          </div>
        ) : mediaUrl ? (
          <>
            {media.map((entry) => {
              const style = {
                "--display-aspect": String(entry.displayAspect),
                "--inverse-display-aspect": String(1 / entry.displayAspect),
                "--viewer-rotation": `${entry.rotation}deg`,
              } as CSSProperties;
              // A poster drawn from a thumbnail whose orientation disagrees
              // with the media would flash the frame the wrong way up before
              // the first video frame paints, so it is dropped instead.
              const posterAgrees = isRawOrientationThumbnail(entry.asset) === entry.mediaIsRawOriented;
              return (
                <div
                  className={`viewer-media ${entry.active ? "active" : "preloaded"} ${entry.rotation % 180 !== 0 ? "quarter-turn" : ""}`}
                  style={style}
                  key={entry.asset.id}
                  aria-hidden={!entry.active}
                >
                  <video
                    ref={entry.active ? videoRef : undefined}
                    data-asset-id={entry.asset.id}
                    data-cache-slot={entry.active ? "current" : entry.offset < 0 ? "previous" : "next"}
                    src={entry.url ?? undefined}
                    poster={entry.active && entry.posterUrl && posterAgrees ? entry.posterUrl : undefined}
                    preload={entry.active || entry.offset > 0 ? "auto" : "metadata"}
                    autoPlay={entry.active && autoplay}
                    muted={muted}
                    onClick={entry.active ? onTogglePlay : undefined}
                    onLoadStart={() => entry.active && onBufferingChange(true)}
                    onWaiting={() => entry.active && onBufferingChange(true)}
                    onCanPlay={() => entry.active && onBufferingChange(false)}
                    onPlaying={() => entry.active && onBufferingChange(false)}
                    onPlay={() => entry.active && onPlayingChange(true)}
                    onPause={() => entry.active && onPlayingChange(false)}
                    onEnded={() => entry.active && onPlayingChange(false)}
                    onLoadedMetadata={(event) => {
                      if (!entry.active) return;
                      onDurationChange(
                        Number.isFinite(event.currentTarget.duration)
                          ? event.currentTarget.duration
                          : entry.asset.durationSeconds,
                      );
                      onPositionChange(event.currentTarget.currentTime);
                    }}
                    onDurationChange={(event) => {
                      if (entry.active && Number.isFinite(event.currentTarget.duration)) {
                        onDurationChange(event.currentTarget.duration);
                      }
                    }}
                    onTimeUpdate={(event) => entry.active && onPositionChange(event.currentTarget.currentTime)}
                  />
                </div>
              );
            })}
            {buffering && <div className="viewer-buffering" role="status"><span /> Buffering</div>}
          </>
        ) : (
          <div className="offline-slate">
            <span className="slate-time">{formatDate(selected.bestTime)}</span>
            <strong>{selected.filename}</strong>
            <span>{selected.proxyPath && !selected.proxyAvailable ? "Proxy is missing" : "No review proxy yet"}</span>
            {canMakeProxy && (
              <button className="button primary" disabled={busy} onClick={onMakeProxy}>Make proxy</button>
            )}
          </div>
        )
      ) : (
        <div className="offline-slate">
          <strong>Choose a clip</strong>
          <span>The next frame will appear here.</span>
        </div>
      )}
      {selected && (
        <div className="viewer-overlay">
          <span>{selected.proxyAvailable ? "PROXY" : "ORIGINAL"}</span>
          <span>{selected.width}×{selected.height} · {selected.frameRate.toFixed(2)} fps</span>
        </div>
      )}
    </div>
  );
}
