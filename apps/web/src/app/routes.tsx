import { Navigate, useParams, type RouteObject } from "react-router";
import { ProjectLayout } from "../features/projects/ProjectLayout";
import { ProjectsPage } from "../features/projects/ProjectsPage";
import { legacyTvReviewPath } from "./paths";
import { NotFound, Root, RouteError } from "./Shell";

function LegacyTvReview() {
  const { "*": splat = "" } = useParams();
  return <Navigate to={legacyTvReviewPath(splat)} replace />;
}

/* Sections load on first visit; the projects list ships with the shell. */
const clips = () => import("../features/clips/ClipsPage").then((m) => ({ Component: m.ClipsPage }));
const library = () => import("../features/library/LibraryPage").then((m) => ({ Component: m.LibraryPage }));
const admin = () => import("../features/admin/AdminPage").then((m) => ({ Component: m.AdminPage }));
const tv = () => import("../features/tvReview/pages");

export const routes: RouteObject[] = [
  {
    path: "/",
    element: <Root />,
    errorElement: <RouteError />,
    children: [
      { index: true, element: <ProjectsPage /> },
      { path: "library", lazy: library },
      { path: "admin", lazy: admin },
      { path: "projects", element: <Navigate to="/" replace /> },
      {
        path: "projects/:projectId",
        element: <ProjectLayout />,
        children: [
          { index: true, lazy: clips },
          { path: "reviews", lazy: () => tv().then((m) => ({ Component: m.TargetsPage })) },
          { path: "reviews/:targetId", lazy: () => tv().then((m) => ({ Component: m.TargetPage })) },
          { path: "reviews/:targetId/record", lazy: () => tv().then((m) => ({ Component: m.RecordPage })) },
          { path: "reviews/:targetId/sessions/:sessionId", lazy: () => tv().then((m) => ({ Component: m.SessionPage })) },
          { path: "reviews/:targetId/sessions/:sessionId/runs/:runId", lazy: () => tv().then((m) => ({ Component: m.WorkspacePage })) },
        ],
      },
      { path: "tv-review/*", element: <LegacyTvReview /> },
      { path: "*", element: <NotFound /> },
    ],
  },
];
