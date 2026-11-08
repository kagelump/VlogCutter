import { Outlet, useParams } from "react-router";
import { paths } from "../../app/paths";
import { Tabs } from "../../ui";
import { projectName } from "./projectName";
import styles from "./Projects.module.css";

export function useProjectId() {
  const { projectId } = useParams();
  if (!projectId) throw new Error("useProjectId outside a project route");
  return projectId;
}

export function ProjectLayout() {
  const projectId = useProjectId();
  return (
    <>
      <div className={styles.bar}>
        <h1 title={projectId}>{projectName(projectId)}</h1>
        <Tabs
          label="Project sections"
          items={[
            { to: paths.project(projectId), label: "Clips", end: true, icon: "film" },
            { to: paths.reviews(projectId), label: "TV reviews", icon: "tv" },
          ]}
        />
      </div>
      <Outlet />
    </>
  );
}
