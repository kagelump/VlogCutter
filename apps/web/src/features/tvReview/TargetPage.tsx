import { useParams } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { keys, useClient } from "../../app/client";
import { paths } from "../../app/paths";
import { Page } from "../../app/Shell";
import { describeCode } from "../../lib/errors";
import { formatClock, formatRelative, shortId } from "../../lib/format";
import { useDocumentTitle } from "../../lib/hooks";
import { Button, ButtonAnchor, ButtonLink, ErrorMessage, Loading, Notice, PageHeader } from "../../ui";
import { useProjectId } from "../projects/ProjectLayout";
import { Recordings } from "./Recordings";
import { HistoricalBadge, PendingRecoveries, ReadinessBadge, isHistorical, targetTitle } from "./shared";
import { ShotGrid } from "./ShotGrid";
import { RenameCut } from "./RenameCut";
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

  const videoId = detail.youtube.videoId;
  return (
    <Page wide>
      <PageHeader
        back={{ to: paths.reviews(projectId), label: "TV reviews" }}
        title={<span className={styles.cutTitle} title={detail.revisionId}>{targetTitle(detail)}</span>}
        titleExtra={<><HistoricalBadge target={detail} /><ReadinessBadge target={detail} /></>}
        subtitle={`${detail.displayName ? `Revision ${shortId(detail.revisionId)} · ` : ""}${formatClock(detail.durationSeconds)} · ${detail.cutCount.toLocaleString()} shots · verified ${formatRelative(detail.latestVerifiedAt)}`}
        actions={<>
          <RenameCut projectId={projectId} target={detail} />
          {videoId ? (
            <ButtonAnchor icon="external" href={`https://www.youtube.com/watch?v=${encodeURIComponent(videoId)}`} target="_blank" rel="noreferrer noopener">YouTube</ButtonAnchor>
          ) : null}
          {videoId && detail.readiness.ready ? <ButtonLink icon="mic" to={`${paths.record(projectId, detail.reviewTargetId)}?mode=play`}>Play + record review</ButtonLink> : null}
          {detail.readiness.ready
            ? <ButtonLink variant="primary" icon="mic" to={paths.record(projectId, detail.reviewTargetId)}>Record review</ButtonLink>
            : <Button variant="primary" icon="mic" disabled>Record review</Button>}
        </>}
      />

      <div className={styles.stack}>
        {isHistorical(detail) ? (
          <Notice tone="warning" title="Historical cut.">Its sources weren’t re-checked, so nothing here can render, edit, activate or upload it.</Notice>
        ) : null}
        {!detail.readiness.ready ? (
          <Notice tone="critical" title="Recording is blocked.">{detail.readiness.blockers.map(describeCode).join(" ")} Existing recordings are kept.</Notice>
        ) : null}
        <PendingRecoveries projectId={projectId} />
        <Recordings projectId={projectId} targetId={detail.reviewTargetId} />
        <ShotGrid projectId={projectId} cuts={detail.cuts} />
      </div>
    </Page>
  );
}
