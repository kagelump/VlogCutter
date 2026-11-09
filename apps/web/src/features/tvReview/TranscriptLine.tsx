import { useState } from "react";
import type { ReviewUtterance, MovieTimeEstimate } from "@rcdvlog/project-client";
import { formatStamp } from "../../lib/format";
import styles from "./TvReview.module.css";

const FLAG_COPY: Record<string, string> = {
  lowConfidence: "low confidence",
  probabilityEvidenceUnavailable: "no confidence data",
  unexpectedLanguage: "unexpected language",
  languageUnknown: "language unknown",
  nearRecordingGap: "near a gap",
};

export function UtteranceLine({ utterance, showRaw, movieTime, thumbnail, shotNumber, videoId, timingLoading }: {
  utterance: ReviewUtterance; showRaw: boolean; movieTime?: MovieTimeEstimate;
  thumbnail?: string; shotNumber?: number; videoId?: string; timingLoading: boolean;
}) {
  return (
    <li className={styles.utterance} data-suppressed={utterance.suppressed || undefined}>
      <span className={styles.stamp}><small>Audio</small><br />{formatStamp(utterance.startSeconds)}</span>
      <span className={styles.speaker}>{utterance.speaker === "unknown" ? "?" : utterance.speaker}</span>
      <span className={styles.utteranceText} lang={utterance.language === "zh" ? "zh-Hant" : utterance.language || undefined}>
        {utterance.suppressed ? <em className={styles.muted}>Low quality: </em> : null}
        {showRaw ? utterance.text : utterance.displayText}
        {utterance.flags.length ? <small className={styles.flags}>{utterance.flags.map((flag) => FLAG_COPY[flag] ?? flag.replace("qualityGate:", "")).join(" · ")}</small> : null}
        <VideoReference movieTime={movieTime} thumbnail={thumbnail} shotNumber={shotNumber} videoId={videoId} loading={timingLoading} />
      </span>
    </li>
  );
}


function VideoReference({ movieTime, thumbnail, shotNumber, videoId, loading }: {
  movieTime?: MovieTimeEstimate; thumbnail?: string; shotNumber?: number; videoId?: string; loading: boolean;
}) {
  if (!movieTime) return <small className={styles.muted}>{loading ? "Matching video time…" : "Video time unavailable"}</small>;
  const label = `Video ${movieTime.source === "humanCorrection" ? "" : "≈ "}${formatStamp(movieTime.movieSeconds)}`;
  return <span className={styles.videoReference}>
    <span className={styles.videoTime} title={movieTime.source === "humanCorrection" ? "Video time set by a reviewer" : `Estimated video time at the start of this line (±${movieTime.uncertaintySeconds.toFixed(2)}s)`}>
      {videoId ? <a href={`https://www.youtube.com/watch?v=${encodeURIComponent(videoId)}&t=${Math.floor(movieTime.movieSeconds)}s`} target="_blank" rel="noopener noreferrer">{label}</a> : label}
    </span>
    {thumbnail ? <TranscriptThumbnail key={thumbnail} src={thumbnail} shotNumber={shotNumber} /> : null}
  </span>;
}

function TranscriptThumbnail({ src, shotNumber }: { src: string; shotNumber?: number }) {
  const [failed, setFailed] = useState(false);
  return <span className={styles.videoThumbnail}>
    {failed ? <small className={styles.muted}>Thumbnail unavailable</small>
      : <><img src={src} alt={`Shot ${shotNumber} thumbnail`} loading="lazy" decoding="async" onError={() => setFailed(true)} />
        <small className={styles.muted}>Shot {shotNumber} thumbnail</small></>}
  </span>;
}
