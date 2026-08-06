import type { Asset, AssetAnalysisView, ClipTranscript } from "../types";
import {
  analysisNeedsAttention,
  formatBytes,
  transcriptBadge,
  transcriptStatusLabels,
} from "../review/summaries";
import { formatDate, formatTimecode } from "../review/format";

export type ShelfTab = "transcript" | "description" | "analysis" | "file";

export interface EvidenceShelfProps {
  asset: Asset;
  transcript: ClipTranscript | null;
  observation: AssetAnalysisView | null;
  open: boolean;
  tab: ShelfTab;
  onTabChange(tab: ShelfTab): void;
  onToggleOpen(): void;
  onSeek(seconds: number): void;
  onChangeAsset(next: Asset): void;
  onCommitAsset(): void;
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
  onTabChange,
  onToggleOpen,
  onSeek,
  onChangeAsset,
  onCommitAsset,
}: EvidenceShelfProps) {
  const segments = transcript?.segments ?? [];
  const badge = transcriptBadge(transcript);
  const attention = analysisNeedsAttention(observation);

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
                {transcript
                  ? transcript.status === "no_speech"
                    ? "No speech in this clip."
                    : transcript.rejectReason || transcriptStatusLabels[transcript.status]
                  : "This clip has not been transcribed."}
              </p>
            ) : (
              <ol className="segment-list">
                {segments.map((segment) => (
                  <li key={`${segment.startMs}-${segment.endMs}`}>
                    <button className="segment" onClick={() => onSeek(segment.startMs / 1000)} title="Jump to this moment">
                      <time>{formatTimecode(segment.startMs)}</time>
                      <span>{segment.text}</span>
                    </button>
                  </li>
                ))}
              </ol>
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
            </dl>
          )}
        </div>
      )}
    </div>
  );
}
