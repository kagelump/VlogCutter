import { shortId } from "../../lib/format";

/** Projects have no display name on the server yet; show a short, stable ID. */
export function projectName(projectId: string) {
  return `Project ${shortId(projectId)}`;
}
