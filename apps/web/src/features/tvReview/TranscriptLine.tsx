import { useState } from "react";
import type { ReviewUtterance, MovieTimeEstimate, MovieTimeRange } from "@rcdvlog/project-client";
import { formatStamp } from "../../lib/format";
import styles from "./TvReview.module.css";

const FLAG_COPY: Record<string, string> = {
  lowConfidence: "low confidence",
  probabilityEvidenceUnavailable: "no confidence data",
  unexpectedLanguage: "unexpected language",
  languageUnknown: "language unknown",
  nearRecordingGap: "near a gap",
};

export function UtteranceLine({ utterance, showRaw, movieTime, movieTimeRanges, thumbnail, shotNumber, videoId }: {
  utterance: ReviewUtterance; showRaw: boolean; movieTime?: MovieTimeEstimate;
  movieTimeRanges?: MovieTimeRange[];
  thumbnail?: string; shotNumber?: number; videoId?: string;
}) {
  return (
    <li className={styles.utterance} data-suppressed={utterance.suppressed || undefined}>
      <span className={styles.stamp}><small>Audio</small><br />{formatStamp(utterance.startSeconds)}</span>
      <span className={styles.speaker}>{utterance.speaker === "unknown" ? "?" : utterance.speaker}</span>
      <span className={styles.utteranceText} lang={utterance.language === "zh" ? "zh-Hant" : utterance.language || undefined}>
        {utterance.suppressed ? <em className={styles.muted}>Low quality: </em> : null}
        {showRaw ? utterance.text : utterance.displayText}
        {utterance.flags.length ? <small className={styles.flags}>{utterance.flags.map((flag) => FLAG_COPY[flag] ?? flag.replace("qualityGate:", "")).join(" · ")}</small> : null}
        <VideoReference movieTime={movieTime} ranges={movieTimeRanges} thumbnail={thumbnail} shotNumber={shotNumber} videoId={videoId} />
      </span>
    </li>
  );
}


function VideoReference({ movieTime, ranges, thumbnail, shotNumber, videoId }: {
  movieTime?: MovieTimeEstimate; thumbnail?: string; shotNumber?: number; videoId?: string;
  ranges?: MovieTimeRange[];
}) {
  if (!thumbnail) return null;
  const seconds = movieTime?.movieSeconds ?? ranges?.[0]?.movieStartSeconds;
  const href = videoId && seconds !== undefined && Number.isFinite(seconds) && seconds >= 0
    ? `https://www.youtube.com/watch?v=${encodeURIComponent(videoId)}&t=${Math.floor(seconds)}s` : undefined;
  const title = seconds === undefined ? undefined : `Video ${formatStamp(seconds)}${!movieTime ? " · first observed playback in this line" : ""}`;
  return <TranscriptThumbnail key={thumbnail} src={thumbnail} shotNumber={shotNumber} href={href} title={title} />;
}

function TranscriptThumbnail({ src, shotNumber, href, title }: { src: string; shotNumber?: number; href?: string; title?: string }) {
  const [failed, setFailed] = useState(false);
  if (failed) return null;
  const image = <img src={src} alt={shotNumber === undefined ? "Video thumbnail" : `Shot ${shotNumber} thumbnail`} loading="lazy" decoding="async" onError={() => setFailed(true)} />;
  return <span className={styles.videoReference}>
    {href ? <a className={styles.videoThumbnail} href={href} title={title} target="_blank" rel="noopener noreferrer">{image}</a>
      : <span className={styles.videoThumbnail} title={title}>{image}</span>}
  </span>;
}
