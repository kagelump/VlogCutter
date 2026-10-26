import { useCallback, useEffect, useRef, useState } from "react";
import type { Asset, AssetAnalysisView, ClipTranscript } from "../types";
import {
  activeSegmentIndex,
  followScrollTop,
  FOLLOW_SUSPENSION_MS,
} from "../review/transcriptFollow";
import {
  analysisNeedsAttention,
  formatBytes,
  mediaAvailability,
  mediaAvailabilityDescription,
  transcriptBadge,
  transcriptStatusLabels,
} from "../review/summaries";
import { formatDate, formatTimecode } from "../review/format";
import { transcriptMatches } from "../review/searchMatch";

export type ShelfTab = "transcript" | "description" | "analysis" | "file";

export interface EvidenceShelfProps {
  asset: Asset;
  transcript: ClipTranscript | null;
  observation: AssetAnalysisView | null;
  open: boolean;
  tab: ShelfTab;
  onTabChange(tab: ShelfTab): void;
  onToggleOpen(): void;
  /** Where playback is, so the transcript can show what is being said now. */
  positionSeconds: number;
  /** The queue's search, so a spoken match is visible in the transcript too. */
  query: string;
  onSeek(seconds: number): void;
  onChangeAsset(next: Asset): void;
  onCommitAsset(): void;
  /** Reconnect this clip's originals: the relink workflow, from the clip that needs it. */
  onLocateOriginals(): void;
  /** An operation already has the project; the relink dialog must not stack on it. */
  busy: boolean;
}

/**
 * Machine output, under the player (spec §3.6).
 *
 * Two reasons it lives here rather than in the right rail. Transcript
 * segments are seek controls — clicking one moves the playhead — so putting
 * them 700px from the transport would be the actual mistake. And at 740px a
 * segment fits on one line where at 400px it wrapped to three, which is where
 * half the old inspector's height went.
 *
 * Collapsed by default: the tab strip stays visible with its badges, so
 * nothing is hidden, and the video stage keeps its full height until you ask.
 */
export function EvidenceShelf({
  asset,
  transcript,
  observation,
  open,
  tab,
  positionSeconds,
  query,
  onTabChange,
  onToggleOpen,
  onSeek,
  onChangeAsset,
  onCommitAsset,
  onLocateOriginals,
  busy,
}: EvidenceShelfProps) {
  const segments = transcript?.segments ?? [];
  const reviewRequired = transcript?.status === "review_required";
  const nativeCoverageIncomplete =
    transcript?.outcomeReason === "native_coverage_incomplete";
  const badge = transcriptBadge(transcript);
  const attention = analysisNeedsAttention(observation);
  const activeSegment = activeSegmentIndex(segments, positionSeconds);
  // A clip reached through search should show where the phrase was said, not
  // make the reader scan the transcript that already found it.
  const matched = new Set(transcriptMatches(segments, query));

  // Auto-follow, and the rule for getting out of its way (issue #13).
  const listRef = useRef<HTMLOListElement>(null);
  const [followSuspended, setFollowSuspended] = useState(false);
  const suspensionTimer = useRef(0);
  // A scroll this component performed is not the reader scrolling away from
  // the playhead, and must not suspend following.
  const selfScrolling = useRef(false);

  const resumeFollowing = useCallback(() => {
    window.clearTimeout(suspensionTimer.current);
    suspensionTimer.current = 0;
    setFollowSuspended(false);
  }, []);

  // Another clip is another transcript; nothing carries over.
  useEffect(resumeFollowing, [asset.id, resumeFollowing]);

  useEffect(() => {
    if (!open || tab !== "transcript" || followSuspended || activeSegment < 0) return;
    const list = listRef.current;
    const row = list?.children[activeSegment];
    if (!list || !(row instanceof HTMLElement)) return;
    const next = followScrollTop({
      itemTop: row.offsetTop - list.offsetTop,
      itemHeight: row.offsetHeight,
      scrollTop: list.scrollTop,
      viewportHeight: list.clientHeight,
    });
    if (next === null) return;
    selfScrolling.current = true;
    list.scrollTop = next;
  }, [activeSegment, followSuspended, open, tab]);

  // Nothing may be left ticking against an unmounted shelf.
  useEffect(() => () => window.clearTimeout(suspensionTimer.current), []);

  function onSegmentListScroll() {
    if (selfScrolling.current) {
      selfScrolling.current = false;
      return;
    }
    // Reading elsewhere is a legitimate thing to be doing; following gives way
    // for a bounded time rather than fighting for the scroll position. Each
    // further scroll restarts the clock, so a long read is not interrupted.
    window.clearTimeout(suspensionTimer.current);
    suspensionTimer.current = window.setTimeout(resumeFollowing, FOLLOW_SUSPENSION_MS);
    setFollowSuspended(true);
  }

  function seekToSegment(startMs: number) {
    resumeFollowing();
    onSeek(startMs / 1000);
  }

  const tabs: Array<{ id: ShelfTab; label: string; badge?: string; attention?: boolean }> = [
    { id: "transcript", label: "Transcript", badge: badge === null ? undefined : String(badge) },
    { id: "description", label: "Description" },
    { id: "analysis", label: "Analysis", attention },
    { id: "file", label: "File" },
  ];

  return (
    <div className="evidence-shelf">
      <div className="shelf-tabs" role="tablist" aria-label="Clip evidence">
        {tabs.map((entry) => (
          <button
            key={entry.id}
            className="shelf-tab"
            role="tab"
            aria-selected={open && tab === entry.id}
            data-attention={entry.attention ? "true" : undefined}
            onClick={() => (open && tab === entry.id ? onToggleOpen() : (onTabChange(entry.id), open || onToggleOpen()))}
          >
            {entry.label}
            {entry.badge && <span className="shelf-badge">{entry.badge}</span>}
          </button>
        ))}
        <button className="shelf-toggle" onClick={onToggleOpen} aria-keyshortcuts="D" aria-expanded={open}>
          {open ? "Hide" : "Show"} <kbd>D</kbd>
        </button>
      </div>

      {open && (
        <div className="shelf-pane" role="tabpanel">
          {tab === "transcript" && (
            segments.length === 0 ? (
              <p className="pane-empty">
                {transcript ? (
                  transcript.status === "no_speech" ? (
                    transcript.outcomeReason === "legacy_no_speech_unverified"
                      ? "This older result could not verify silence. Transcribe again to check."
                      : "No speech in this clip."
                  ) : transcript.status === "no_audio" ? (
                    "This video has no audio stream."
                  ) : transcript.status === "no_accepted_speech" ? (
                    "Speech was detected, but the transcriber returned no usable text."
                  ) : transcript.status === "review_required" ? (
                    "This transcript needs review because quality evidence is incomplete; no candidate text was retained."
                  ) : transcript.outcomeReason === "native_coverage_incomplete" ? (
                    "Transcription is incomplete; retained candidate text is unaccepted."
                  ) : (
                    transcript.rejectReason || transcriptStatusLabels[transcript.status]
                  )
                ) : (
                  "This clip has not been transcribed."
                )}
              </p>
            ) : (
              <>
                {reviewRequired && (
                  <div
                    className="transcript-review-warning"
                    role="note"
                    aria-label="Transcript requires review"
                  >
                    <strong>Transcript needs review.</strong> Quality evidence is incomplete.
                    Check these candidate lines against the clip before using them.
                  </div>
                )}
                {nativeCoverageIncomplete && (
                  <div
                    className="transcript-review-warning"
                    role="note"
                    aria-label="Transcription incomplete"
                  >
                    <strong>Transcription is incomplete.</strong> Retained candidate text is
                    unaccepted and must not be treated as a full transcript.
                  </div>
                )}
                {followSuspended && (
                  <button className="segment-follow" onClick={resumeFollowing}>
                    Follow playback
                  </button>
                )}
                <ol className="segment-list" ref={listRef} onScroll={onSegmentListScroll}>
                  {segments.map((segment, position) => (
                    <li
                      key={`${segment.startMs}-${segment.endMs}`}
                      className={`${position === activeSegment ? "active" : ""} ${matched.has(position) ? "matched" : ""}`.trim()}
                    >
                      <button
                        className="segment"
                        aria-current={position === activeSegment ? "true" : undefined}
                        onClick={() => seekToSegment(segment.startMs)}
                        title="Jump to this moment"
                      >
                        <time>{formatTimecode(segment.startMs)}</time>
                        <span>{segment.text}</span>
                      </button>
                    </li>
                  ))}
                </ol>
              </>
            )
          )}

          {tab === "description" && (
            <textarea
              className="description-field"
              value={asset.description}
              placeholder="No description yet."
              aria-label="Clip description"
              onChange={(event) => onChangeAsset({ ...asset, description: event.target.value })}
              onBlur={onCommitAsset}
            />
          )}

          {tab === "analysis" && (
            observation ? (
              <dl className="fact-grid">
                <div>
                  <dt>Verdict</dt>
                  <dd>
                    {observation.analysis.usable
                      ? "Usable"
                      : `Unusable — ${observation.analysis.unusableReason ?? "no reason recorded"}`}
                  </dd>
                </div>
                <div><dt>Camera movement</dt><dd>{observation.analysis.cameraMovement}</dd></div>
                <div><dt>Speech used</dt><dd>{observation.analysis.transcriptUsed ? "yes" : "no"}</dd></div>
                <div><dt>Best moment</dt><dd>
                  {observation.analysis.bestMomentStartMs === null
                    ? "none found"
                    : `${formatTimecode(observation.analysis.bestMomentStartMs)} · press B`}
                </dd></div>
                {observation.aiLabels.length > 0 && (
                  <div className="fact-wide"><dt>Suggested labels</dt><dd>{observation.aiLabels.join(" · ")}</dd></div>
                )}
                {observation.analysis.ocrText.length > 0 && (
                  <div className="fact-wide"><dt>Text on screen</dt><dd className="mono">{observation.analysis.ocrText.join(" · ")}</dd></div>
                )}
                <div className="fact-wide">
                  <dt>Model</dt>
                  <dd>
                    {observation.analysis.model}
                    {observation.current ? "" : " · stale, the proxy changed after this ran"}
                  </dd>
                </div>
              </dl>
            ) : <p className="pane-empty">This clip has not been analyzed.</p>
          )}

          {tab === "file" && (
            <dl className="fact-grid">
              <div className="fact-wide"><dt>File</dt><dd className="mono">{asset.filename}</dd></div>
              <div><dt>Camera</dt><dd>{asset.camera}</dd></div>
              <div><dt>Format</dt><dd>{asset.codec.toUpperCase()} · {formatBytes(asset.sizeBytes)}</dd></div>
              <div><dt>Frame</dt><dd>{asset.width}×{asset.height} · {asset.frameRate.toFixed(2)} fps</dd></div>
              <div><dt>Camera clock</dt><dd className="mono">{formatDate(asset.capturedAtRaw)}</dd></div>
              <div><dt>Corrected</dt><dd className="mono">{formatDate(asset.bestTime)}</dd></div>
              <div><dt>Original</dt><dd>{asset.originalAvailable ? "mounted" : "offline"}</dd></div>
              <div><dt>Proxy</dt><dd>{asset.proxyAvailable ? "720p present" : "none"}</dd></div>
              {/* Repair belongs where the problem is visible. Reaching relink
                  only through the chronology desk's diagnostics made it
                  something you had to already know about. */}
              {mediaAvailability(asset) !== "available" && (
                <div className="fact-wide">
                  <dt>Offline</dt>
                  <dd>
                    {mediaAvailabilityDescription(asset)}
                    <button className="button ghost" disabled={busy} onClick={onLocateOriginals}>
                      Locate originals…
                    </button>
                  </dd>
                </div>
              )}
            </dl>
          )}
        </div>
      )}
    </div>
  );
}
