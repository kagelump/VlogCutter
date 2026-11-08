import { Link } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { keys, useClient } from "../../app/client";
import { paths } from "../../app/paths";
import { Page } from "../../app/Shell";
import { formatRelative, plural } from "../../lib/format";
import { useDocumentTitle } from "../../lib/hooks";
import { Badge, EmptyState, ErrorMessage, Icon, Loading, PageHeader } from "../../ui";
import { projectName } from "./projectName";
import styles from "./Projects.module.css";

export function ProjectsPage() {
  const client = useClient();
  const projects = useQuery({ queryKey: keys.projects, queryFn: () => client.listProjects(100) });
  useDocumentTitle("Projects");

  return (
    <Page>
      <PageHeader title="Projects" />
      {projects.isPending ? <Loading /> : null}
      <ErrorMessage error={projects.error} onRetry={() => void projects.refetch()} />
      {projects.data && !projects.data.items.length ? (
        <EmptyState title="No projects yet">Projects appear here once a Mac shares one with this server.</EmptyState>
      ) : null}
      <ul className={styles.grid}>
        {projects.data?.items.map((project) => (
          <li key={project.projectId} className={styles.card}>
            <Link to={paths.project(project.projectId)} className={styles.cardMain}>
              <h2 title={project.projectId}>{projectName(project.projectId)}</h2>
              <p>{plural(project.assetCount, "clip")} · updated {formatRelative(project.updatedAt)}</p>
              {project.state !== "registered" ? <Badge tone="warning">{project.state}</Badge> : null}
            </Link>
            <div className={styles.cardLinks}>
              <Link to={paths.project(project.projectId)}><Icon name="film" size={16} />Clips</Link>
              <Link to={paths.reviews(project.projectId)}><Icon name="tv" size={16} />TV reviews</Link>
            </div>
          </li>
        ))}
      </ul>
    </Page>
  );
}
