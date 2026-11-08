import { useQuery } from "@tanstack/react-query";
import { keys, useClient } from "../../app/client";
import { humanize } from "../../lib/errors";
import { formatRelative, plural } from "../../lib/format";
import { Badge, ErrorMessage, Loading } from "../../ui";
import { projectName } from "../projects/projectName";
import { ADMIN_POLL_MS, Row, Rows, Section } from "./shared";

export function SyncSection() {
  const client = useClient();
  const sync = useQuery({ queryKey: [...keys.admin, "sync"], queryFn: () => client.listAdminSync(), refetchInterval: ADMIN_POLL_MS });
  return (
    <Section id="sync" title="Sync" description="Projects shared from Macs and their sync state.">
      {sync.isPending ? <Loading /> : null}
      <ErrorMessage error={sync.error} />
      {sync.data ? (
        <Rows empty="No projects are syncing.">
          {sync.data.items.map((project) => (
            <Row
              key={project.projectId}
              title={<span title={project.projectId}>{projectName(project.projectId)}</span>}
              meta={[
                `from ${project.originNodeId ?? "unknown Mac"}`,
                plural(project.operationCount, "change"),
                `last synced ${formatRelative(project.lastSuccessAt)}`,
                project.pendingProjectionRepairs ? `${project.pendingProjectionRepairs} repairs pending` : null,
                project.lastErrorCode ? `last error ${project.lastErrorCode}` : null,
              ].filter(Boolean).join(" · ")}
              status={<>
                <Badge tone={project.connectivity === "online" || project.connectivity === "connected" ? "positive" : "neutral"}>{humanize(project.connectivity)}</Badge>
                {project.unresolvedConflictCount
                  ? <Badge tone="warning" title={project.recommendations.join("\n")}>{plural(project.unresolvedConflictCount, "conflict")}</Badge>
                  : project.compatibility !== "compatible" ? <Badge tone="warning">{humanize(project.compatibility)}</Badge> : null}
              </>}
            />
          ))}
        </Rows>
      ) : null}
    </Section>
  );
}
