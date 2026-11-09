import { useState } from "react";
import { Link, useNavigate } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { keys, useClient } from "../../app/client";
import { paths } from "../../app/paths";
import { Page } from "../../app/Shell";
import { formatClock, formatDate, shortId } from "../../lib/format";
import { useDocumentTitle } from "../../lib/hooks";
import { Badge, Button, EmptyState, ErrorMessage, Icon, Loading } from "../../ui";
import { useProjectId } from "../projects/ProjectLayout";
import { ImportDialog } from "./ImportDialog";
import { HistoricalBadge, PendingRecoveries, ReadinessBadge, targetTitle } from "./shared";
import styles from "./TvReview.module.css";

export function TargetsPage() {
  const projectId = useProjectId();
  const client = useClient();
  const navigate = useNavigate();
  const [importing, setImporting] = useState(false);
  const targets = useQuery({ queryKey: keys.targets(projectId), queryFn: () => client.listReviewTargets(projectId, 50) });
  useDocumentTitle("TV reviews");
  const items = targets.data?.items ?? [];

  return (
    <Page>
      <div className={styles.toolbar}>
        <p className={styles.intro}>Watch a rendered cut on the TV and record your reactions. Each cut is imported from the Mac.</p>
        <Button icon="upload" onClick={() => setImporting(true)}>Import cut</Button>
      </div>

      <PendingRecoveries projectId={projectId} />
      {targets.isPending ? <Loading /> : null}
      <ErrorMessage error={targets.error} onRetry={() => void targets.refetch()} />
      {targets.isSuccess && !items.length ? (
        <EmptyState title="No cuts to review yet" action={<Button variant="primary" icon="upload" onClick={() => setImporting(true)}>Import cut</Button>}>
          On the Mac, run <code>rcdvlog edit-plan review-target-export --artifact &lt;preview-id&gt;</code> and import the result here.
        </EmptyState>
      ) : null}

      {items.length ? (
        <ul className={styles.list}>
          {items.map((target) => (
            <li key={target.reviewTargetId}>
              <Link to={paths.target(projectId, target.reviewTargetId)} className={styles.listItem}>
                <span className={styles.listMain}>
                  <strong title={target.revisionId}>{targetTitle(target)}</strong>
                  <span>{target.displayName ? `Revision ${shortId(target.revisionId)} · ` : ""}{formatClock(target.durationSeconds)} · {target.cutCount.toLocaleString()} shots · imported {formatDate(target.importedAt)}</span>
                </span>
                <span className={styles.listBadges}>
                  {target.youtube.videoId ? <Badge tone="accent">YouTube</Badge> : null}
                  <HistoricalBadge target={target} />
                  <ReadinessBadge target={target} />
                </span>
                <Icon name="chevronRight" className={styles.listChevron} />
              </Link>
            </li>
          ))}
        </ul>
      ) : null}

      <ImportDialog
        open={importing}
        projectId={projectId}
        onClose={() => setImporting(false)}
        onImported={(targetId) => { setImporting(false); navigate(paths.target(projectId, targetId)); }}
      />
    </Page>
  );
}
