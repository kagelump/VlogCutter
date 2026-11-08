import { useQuery } from "@tanstack/react-query";
import { keys, useClient } from "../../app/client";
import { Page } from "../../app/Shell";
import { describeCheck } from "../../lib/errors";
import { formatBytes, plural } from "../../lib/format";
import { useDocumentTitle, useLiveEvents } from "../../lib/hooks";
import { Badge, ErrorMessage, Loading, Notice, PageHeader, Stats } from "../../ui";
import { AccessSection } from "./AccessSection";
import { ActivitySection } from "./ActivitySection";
import { JobsSection } from "./JobsSection";
import { ScansSection } from "./ScansSection";
import { ADMIN_POLL_MS } from "./shared";
import { StorageSection } from "./StorageSection";
import { SyncSection } from "./SyncSection";
import styles from "./Admin.module.css";

const SECTIONS = [
  ["storage", "Storage"],
  ["scans", "Scans"],
  ["jobs", "Jobs"],
  ["sync", "Sync"],
  ["access", "Access"],
  ["activity", "Activity"],
] as const;

export function AdminPage() {
  const client = useClient();
  useDocumentTitle("Admin");
  useLiveEvents("__admin__", keys.admin);
  const overview = useQuery({ queryKey: [...keys.admin, "overview"], queryFn: () => client.getAdminOverview(), refetchInterval: ADMIN_POLL_MS });
  const data = overview.data;
  const failing = data?.readiness.checks.filter((check) => check.status !== "ok") ?? [];

  return (
    <Page>
      <PageHeader
        title="Admin"
        titleExtra={data ? <Badge tone={failing.length ? "warning" : "positive"}>{failing.length ? "Needs attention" : "All checks pass"}</Badge> : null}
        subtitle={data ? <>Server {data.applicationVersion} · node <span title={data.nodeId}>{data.nodeId}</span></> : null}
      />
      <nav className={styles.sectionNav} aria-label="Admin sections">
        {SECTIONS.map(([id, label]) => <a key={id} href={`#${id}`}>{label}</a>)}
      </nav>

      <div className={styles.stack}>
        {overview.isPending ? <Loading /> : null}
        <ErrorMessage error={overview.error} onRetry={() => void overview.refetch()} />
        {data ? (
          <>
            {failing.length ? (
              <Notice tone="warning" title={`${failing.length} failing ${failing.length === 1 ? "check" : "checks"}:`}>
                {failing.map(describeCheck).join(" · ")}
              </Notice>
            ) : null}
            <Stats label="Server overview" items={[
              { label: "Health checks", value: `${data.readiness.checks.length - failing.length} / ${data.readiness.checks.length}`, detail: "passing" },
              { label: "Preview storage free", value: formatBytes(data.derivedAvailableBytes), detail: data.config.derivedDir },
              { label: "Backup storage free", value: formatBytes(data.backupAvailableBytes), detail: data.config.backupDir },
              { label: "Signed in", value: data.auth.activeSessions + data.auth.activeDeviceCredentials, detail: `${plural(data.auth.activeSessions, "browser")} · ${plural(data.auth.activeDeviceCredentials, "Mac")}` },
            ]} />
          </>
        ) : null}

        <StorageSection allowedMounts={data?.config.allowedOriginalMounts ?? null} />
        <ScansSection />
        <JobsSection />
        <SyncSection />
        <AccessSection />
        <ActivitySection />
      </div>
    </Page>
  );
}
