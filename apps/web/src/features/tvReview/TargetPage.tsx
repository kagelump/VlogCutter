import { useState, type FormEvent } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { ReviewTargetDetail } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { paths } from "../../app/paths";
import { Page } from "../../app/Shell";
import { describeCode } from "../../lib/errors";
import { formatBytes, formatClock, formatDateTime, formatMovieTime, formatRelative, plural, shortDigest } from "../../lib/format";
import { useDocumentTitle } from "../../lib/hooks";
import { Button, ButtonLink, Disclosure, ErrorMessage, Facts, Field, Icon, Loading, Notice, PageHeader, Panel } from "../../ui";
import { useProjectId } from "../projects/ProjectLayout";
import { HistoricalBadge, PendingRecoveries, ReadinessBadge, SessionBadge, isHistorical, targetTitle } from "./shared";
import { parseYoutubeVideoId } from "./youtube";
import styles from "./TvReview.module.css";

export function TargetPage() {
  const projectId = useProjectId();
  const { targetId = "" } = useParams();
  const client = useClient();
  const target = useQuery({ queryKey: keys.target(projectId, targetId), queryFn: () => client.getReviewTarget(projectId, targetId) });
  const detail = target.data;
  useDocumentTitle(detail ? targetTitle(detail) : "TV review");

  if (target.isPending) return <Page><Loading /></Page>;
  if (!detail) return <Page><PageHeader title="TV review" back={{ to: paths.reviews(projectId), label: "TV reviews" }} /><ErrorMessage error={target.error} onRetry={() => void target.refetch()} /></Page>;

  return (
    <Page>
      <PageHeader
        back={{ to: paths.reviews(projectId), label: "TV reviews" }}
        title={<span title={detail.revisionId}>{targetTitle(detail)}</span>}
        titleExtra={<><HistoricalBadge target={detail} /><ReadinessBadge target={detail} /></>}
        subtitle={`${formatClock(detail.durationSeconds)} · ${detail.cutCount.toLocaleString()} shots · verified ${formatRelative(detail.latestVerifiedAt)}`}
        actions={detail.readiness.ready
          ? <ButtonLink variant="primary" icon="mic" to={paths.record(projectId, detail.reviewTargetId)}>Record review</ButtonLink>
          : <Button variant="primary" icon="mic" disabled>Record review</Button>}
      />

      <div className={styles.columns}>
        <div className={styles.mainColumn}>
          {isHistorical(detail) ? (
            <Notice tone="warning" title="Historical cut.">Talk about what this rendered movie shows. Its sources and renderability weren’t re-checked, so nothing here can render, edit, activate or upload it.</Notice>
          ) : null}
          {!detail.readiness.ready ? (
            <Notice tone="critical" title="Recording is blocked.">{detail.readiness.blockers.map(describeCode).join(" ")} Existing recordings are kept.</Notice>
          ) : null}
          <PendingRecoveries projectId={projectId} />
          <Recordings projectId={projectId} targetId={detail.reviewTargetId} />
          <Disclosure title="Shots" meta={detail.cutCount.toLocaleString()}>
            <ShotTable detail={detail} />
          </Disclosure>
        </div>
        <aside className={styles.sideColumn}>
          <YoutubePanel projectId={projectId} detail={detail} />
          <Disclosure title="Details">
            <TargetFacts projectId={projectId} detail={detail} />
          </Disclosure>
          {detail.limitations.length ? (
            <Disclosure title="What this doesn’t verify">
              <ul className={styles.bullets}>{detail.limitations.map((line) => <li key={line}>{line}</li>)}</ul>
            </Disclosure>
          ) : null}
        </aside>
      </div>
    </Page>
  );
}

function Recordings({ projectId, targetId }: { projectId: string; targetId: string }) {
  const client = useClient();
  const sessions = useQuery({ queryKey: keys.sessions(projectId, targetId), queryFn: () => client.listReviewSessions(projectId, targetId, 50) });
  const items = sessions.data?.items ?? [];
  return (
    <Panel title="Recordings">
      {sessions.isPending ? <Loading /> : null}
      <ErrorMessage error={sessions.error} />
      {sessions.isSuccess && !items.length ? <p className={styles.muted}>No recordings yet. Start one while this cut plays on the TV.</p> : null}
      {items.length ? (
        <ul className={styles.plainList}>
          {items.map((session) => (
            <li key={session.sessionId}>
              <Link to={paths.session(projectId, targetId, session.sessionId)} className={styles.plainItem}>
                <span className={styles.listMain}>
                  <strong>{formatDateTime(session.createdAt)}</strong>
                  <span>{formatClock(session.receivedSeconds)} recorded{session.manualCueCount ? ` · ${plural(session.manualCueCount, "start mark")}` : ""}</span>
                </span>
                <SessionBadge session={session} />
                <Icon name="chevronRight" className={styles.listChevron} />
              </Link>
            </li>
          ))}
        </ul>
      ) : null}
    </Panel>
  );
}

function YoutubePanel({ projectId, detail }: { projectId: string; detail: ReviewTargetDetail }) {
  const client = useClient();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [link, setLink] = useState("");
  const [editing, setEditing] = useState(false);
  const [invalid, setInvalid] = useState(false);
  const videoId = detail.youtube.videoId;
  const declare = useMutation({
    mutationFn: (id: string | null) => client.declareReviewTargetYoutube(projectId, detail.reviewTargetId, id),
    onSuccess: (result) => {
      setLink(""); setEditing(false);
      void queryClient.invalidateQueries({ queryKey: keys.targets(projectId) });
      navigate(paths.target(projectId, result.reviewTarget.reviewTargetId));
    },
  });

  function submit(event: FormEvent) {
    event.preventDefault();
    const id = parseYoutubeVideoId(link);
    setInvalid(!id);
    if (id) declare.mutate(id);
  }

  return (
    <Panel title="YouTube">
      {videoId && !editing ? (
        <div className={styles.youtube}>
          <a href={`https://www.youtube.com/watch?v=${encodeURIComponent(videoId)}`} target="_blank" rel="noreferrer noopener">
            youtu.be/{videoId}<Icon name="external" size={14} />
          </a>
          <p className={styles.muted}>Added {detail.youtube.declaredBy === "export" ? "on the Mac" : "here"}. Not verified to be this exact cut.</p>
          <div className={styles.inlineActions}>
            <Button size="sm" onClick={() => setEditing(true)}>Change</Button>
            <Button size="sm" variant="ghost" busy={declare.isPending && declare.variables === null} onClick={() => declare.mutate(null)}>Remove</Button>
          </div>
        </div>
      ) : (
        <form className={styles.youtubeForm} onSubmit={submit}>
          <Field label="Video link" hint="The video you’ll play on the TV, for reference. It’s never fetched.">
            {(id) => <input id={id} value={link} spellCheck={false} placeholder="https://youtu.be/…" onChange={(event) => { setLink(event.target.value); setInvalid(false); }} />}
          </Field>
          {invalid ? <Notice tone="critical">That isn’t a YouTube link or video ID.</Notice> : null}
          <div className={styles.inlineActions}>
            <Button type="submit" variant="primary" size="sm" busy={declare.isPending} disabled={!link.trim()}>Save link</Button>
            {editing ? <Button size="sm" variant="ghost" onClick={() => setEditing(false)}>Cancel</Button> : null}
          </div>
        </form>
      )}
      <ErrorMessage error={declare.error} />
      <p className={styles.hintSmall}>Changing the link saves a new copy of this cut; recordings stay with the original.</p>
    </Panel>
  );
}

function TargetFacts({ projectId, detail }: { projectId: string; detail: ReviewTargetDetail }) {
  const latest = detail.observations[detail.observations.length - 1];
  return (
    <Facts items={[
      ["Revision", <span className={styles.wrap}>{detail.revisionId}</span>],
      ["Type", isHistorical(detail) ? "Historical: file integrity and cut list only" : "Current: output, sources, constraints and full decode verified"],
      ["Frames", `${detail.movie.videoFrameCount.toLocaleString()} (picture ends ${formatMovieTime(detail.mapping.pictureEndSeconds)})`],
      ["Cut list", detail.mapping.status === "measuredComplete" ? "Measured from the rendered movie" : detail.mapping.status],
      ["Verified", `${formatDateTime(detail.latestVerifiedAt)} · ${detail.readiness.freshness}`],
      ["Movie file", `${detail.readiness.availabilityState}${latest ? ` (${latest.availabilitySource})` : ""} · ${formatBytes(detail.movie.byteLength)}`],
      ["Movie digest", <code title={detail.movie.blake3}>{shortDigest(detail.movie.blake3)}</code>],
      ["Cut digest", <code title={detail.targetDigest}>{shortDigest(detail.targetDigest)}</code>],
      ["Imported", `${formatDateTime(detail.provenance.importedAt)}${detail.provenance.declaredIssuer ? ` from “${detail.provenance.declaredIssuer}”` : ""}`],
      detail.provenance.derivedFromReviewTargetId ? ["Copied from", <Link to={paths.target(projectId, detail.provenance.derivedFromReviewTargetId)}>Version without this YouTube link</Link>] : null,
    ]} />
  );
}

function ShotTable({ detail }: { detail: ReviewTargetDetail }) {
  return (
    <div className={styles.tableScroll}>
      <table className={styles.table}>
        <thead><tr><th scope="col">#</th><th scope="col">Movie</th><th scope="col">Source clip</th><th scope="col">Source range</th><th scope="col">Audio</th></tr></thead>
        <tbody>
          {detail.cuts.map((cut) => (
            <tr key={cut.operationId} title={cut.operationId}>
              <td>{cut.position + 1}</td>
              <td>{formatMovieTime(cut.movieInSeconds)}–{formatMovieTime(cut.movieOutSeconds)}</td>
              <td className={styles.wrap}>{cut.assetId}</td>
              <td>{cut.sourceInSeconds.toFixed(2)}–{cut.sourceOutSeconds.toFixed(2)}s</td>
              <td>{cut.declaredAudio === null ? "Clip" : cut.declaredAudio.mode === "silent" ? "Silent" : "Separate"}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
