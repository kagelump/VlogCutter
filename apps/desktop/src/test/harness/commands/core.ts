/**
 * Application shell commands: prerequisites, catalogs, the project library,
 * desktop settings, and the native dialog seam.
 */

import type { DesktopSettings, LoadedCatalog, MediaOrigin, Project, ProjectLibrary } from "../../../types";
import { buildProject } from "../builders";
import { projectSummary, type DesktopHarnessState } from "../state";
import type { CommandTable } from "../registry";
import { requireString } from "../registry";

function folderPathFor(state: DesktopHarnessState, project: Project): string {
  return `${state.settings.projectsRoot}/${project.name}`;
}

function library(state: DesktopHarnessState): ProjectLibrary {
  return {
    rootPath: state.settings.projectsRoot,
    projects: state.projects.map((project) => {
      const folderPath = folderPathFor(state, project);
      return {
        folderPath,
        catalogPath: `${folderPath}/catalog.sqlite3`,
        project: projectSummary(state, project),
      };
    }),
  };
}

/** Settings always carry the media origin, so `api.mediaUrl` builds real URLs. */
function settingsView(state: DesktopHarnessState): DesktopSettings {
  return { ...state.settings, mediaOrigin: state.mediaOrigin };
}

export const coreCommands: CommandTable = {
  prerequisite_status: (_args, { state }) => state.prerequisites,

  list_projects: (_args, { state }) => state.projects.map((project) => projectSummary(state, project)),

  project_library: (_args, { state }) => library(state),

  load_catalog: (args, { state }): LoadedCatalog => {
    const path = requireString(args, "path");
    state.catalogPath = path;
    return { path, projects: state.projects.map((project) => projectSummary(state, project)) };
  },

  set_projects_root: (args, { state }): ProjectLibrary => {
    state.settings.projectsRoot = requireString(args, "path");
    return library(state);
  },

  desktop_settings: (_args, { state }) => settingsView(state),

  set_autoplay_mode: (args, { state }) => {
    const mode = requireString(args, "autoplayMode");
    if (mode !== "never" && mode !== "proxyAvailable" && mode !== "always") {
      throw new Error(`Unknown autoplay mode: ${mode}`);
    }
    state.settings.autoplayMode = mode;
    return settingsView(state);
  },

  record_desktop_context: (args, { state, clock }) => {
    const projectId = requireString(args, "projectId");
    const assetId = args.assetId;
    if (assetId !== null && typeof assetId !== "string") {
      throw new Error(`record_desktop_context assetId must be a string or null, received ${JSON.stringify(assetId)}`);
    }
    const project = state.projects.find((candidate) => candidate.id === projectId);
    state.settings.lastProjectId = projectId;
    state.settings.lastAssetId = assetId;
    state.settings.lastCatalogPath = project
      ? `${folderPathFor(state, project)}/catalog.sqlite3`
      : state.catalogPath;
    state.settings.lastContextUpdatedAt = clock.iso();
    return settingsView(state);
  },

  create_project: (args, { state, clock }) => {
    const name = requireString(args, "name");
    const project = buildProject({ name, createdAt: clock.iso() });
    if (state.projects.some((candidate) => candidate.id === project.id)) {
      throw new Error(`A project named ${name} already exists`);
    }
    state.projects.push(project);
    return projectSummary(state, project);
  },

  recover_media_origin: (_args, { state }): MediaOrigin | null => state.mediaOrigin,

  /**
   * The native file dialog. A scenario queues what the user picks; an empty
   * queue means they dismissed it. This is the seam, not evidence that macOS
   * dialogs work — that stays a native acceptance item.
   */
  "plugin:dialog|open": (_args, { state }) => state.dialogResponses.shift() ?? null,
};
