import { useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import type { LiveReviewTranscript } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { formatClock } from "../../lib/format";
import { Badge, Button, Notice, Panel } from "../../ui";
import { UtteranceLine } from "./TranscriptLine";
import styles from "./TvReview.module.css";

export function useLiveTranscript(projectId: string, sessionId: string, pollMs = 2_000) {
  const client = useClient();
  return useQuery({
    queryKey: keys.liveTranscript(projectId, sessionId),
    queryFn: () => client.getLiveReviewTranscript(projectId, sessionId),
    enabled: Boolean(sessionId),
    refetchInterval: (query) => !query.state.data || ["live", "refining"].includes(query.state.data.state) ? pollMs : false,
    refetchIntervalInBackground: true,
  });
}

export function LiveTranscriptPanel({ data, projectId, videoId, recording = false }: {
  data: LiveReviewTranscript; projectId: string; videoId?: string; recording?: boolean;
}) {
  const client = useClient();
  const [following, setFollowing] = useState(true);
  const list = useRef<HTMLOListElement>(null);
  const refining = data.state === "refining" || (!recording && data.state === "live");
  useEffect(() => {
    if (following && list.current) list.current.scrollTop = list.current.scrollHeight;
  }, [data.revision, data.lines, following]);
  return <Panel title="Transcript" actions={<Badge tone="warning">{refining ? "Refining" : data.state === "failed" ? "Draft kept" : "Live draft"}</Badge>}>
    <p className={styles.muted}>{refining
      ? "The full recording is being transcribed. Your live draft stays here until the final transcript is ready."
      : data.state === "failed" ? "Your live draft is kept while you retry the full transcription."
      : "Words and video matches update as you speak. The full pass will refine them after you stop."}</p>
    {!data.modelAvailable && recording ? <Notice tone="warning">Live transcription needs a speech model installed on this server. Your audio is still being recorded; the full pass will be attempted after you stop.</Notice> : null}
    {data.errorCode === "review_live_resource_busy" ? <Notice>Live text is waiting for the server to finish another media task. Recording continues.</Notice> : data.errorCode && data.errorCode !== "review_live_model_unavailable" ? <Notice tone="warning">{data.state === "failed" ? "The full pass couldn’t finish. Your draft is kept below. Use the transcription controls to retry." : "Live transcription is delayed. Your audio continues to upload and will be included in the full pass."}</Notice> : null}
    <div className={styles.transcriptBar}>
      <small className={styles.muted}>Draft through audio {formatClock(data.processedSeconds)} · latest {data.lines.length} lines</small>
      <Button size="sm" variant="ghost" onClick={() => setFollowing(!following)} aria-pressed={following}>{following ? "Following live" : "Follow live"}</Button>
    </div>
    {data.lines.length ? <ol ref={list} className={`${styles.utterances} ${styles.liveTranscript}`} aria-label="Live transcript" onScroll={() => {
      const el = list.current;
      if (el && el.scrollHeight - el.clientHeight - el.scrollTop > 48) setFollowing(false);
    }}>
      {data.lines.map((line) => <UtteranceLine key={line.utterance.utteranceId} utterance={line.utterance} showRaw={false}
        movieTime={line.movieTime ?? undefined} thumbnail={line.assetId ? client.projectAssetThumbnailUrl(projectId, line.assetId) : undefined}
        shotNumber={line.shotNumber ?? undefined} videoId={videoId} timingLoading={false} />)}
    </ol> : <p className={styles.muted}>{refining ? "Waiting for the full transcript…" : "Waiting for live text…"}</p>}
  </Panel>;
}
