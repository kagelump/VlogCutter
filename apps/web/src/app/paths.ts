const e = encodeURIComponent;

export const paths = {
  home: () => "/",
  library: () => "/library",
  admin: () => "/admin",
  project: (projectId: string) => `/projects/${e(projectId)}`,
  clip: (projectId: string, assetId: string) => `/projects/${e(projectId)}?clip=${e(assetId)}`,
  reviews: (projectId: string) => `/projects/${e(projectId)}/reviews`,
  target: (projectId: string, targetId: string) => `/projects/${e(projectId)}/reviews/${e(targetId)}`,
  record: (projectId: string, targetId: string) => `${paths.target(projectId, targetId)}/record`,
  session: (projectId: string, targetId: string, sessionId: string) => `${paths.target(projectId, targetId)}/sessions/${e(sessionId)}`,
  workspace: (projectId: string, targetId: string, sessionId: string, runId: string) =>
    `${paths.session(projectId, targetId, sessionId)}/runs/${e(runId)}`,
};

/** Map a pre-redesign `/tv-review/...` address to its new home. */
export function legacyTvReviewPath(splat: string): string {
  const [projectId, targetId, section, sessionId, subsection, runId] = splat.split("/").filter(Boolean).map(decodeURIComponent);
  if (!projectId) return paths.home();
  if (!targetId) return paths.reviews(projectId);
  if (section === "record") return paths.record(projectId, targetId);
  if (section === "sessions" && sessionId) {
    if (subsection === "review" && runId) return paths.workspace(projectId, targetId, sessionId, runId);
    return paths.session(projectId, targetId, sessionId);
  }
  return paths.target(projectId, targetId);
}
