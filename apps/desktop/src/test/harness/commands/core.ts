/**
 * Application shell commands: prerequisites, catalogs, the project library,
 * desktop settings, and the native dialog seam.
 */

import type {
  CacheCleanupReport,
  CacheKind,
  DesktopSettings,
  LoadedCatalog,
  MediaOrigin,
  Project,
  ProjectLibrary,
  ProjectRemovalAudit,
  ProjectStorageReport,
} from "../../../types";
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

function requireProject(state: DesktopHarnessState, projectId: string): Project {
  const project = state.projects.find((found) => found.id === projectId);
  if (!project) throw new Error(`Unknown project: ${projectId}`);
  return project;
}

const CACHE_KINDS: CacheKind[] = [
  "proxies",
  "thumbnails",
  "audio",
  "framePacks",
  "editPreviews",
  "importPreviews",
  "analysisModels",
];

function storageReport(
  state: DesktopHarnessState,
  projectId: string,
  catalogPath: string,
): ProjectStorageReport {
  const stored = state.storage[projectId] ?? {};
  const categories = CACHE_KINDS.map((kind) => ({
    kind,
    scope: (kind === "importPreviews" || kind === "analysisModels" ? "catalog" : "project") as
      "catalog" | "project",
    bytes: stored[kind]?.bytes ?? 0,
    fileCount: stored[kind]?.fileCount ?? 0,
  }));
  return {
    projectId,
    catalogPath,
    catalogBytes: state.catalogBytes[projectId] ?? 2_048,
    categories,
    cacheBytes: categories.reduce((total, category) => total + category.bytes, 0),
  };
}

function removalAudit(
  state: DesktopHarnessState,
  projectId: string,
  catalogPath: string,
  folderPath: string,
): ProjectRemovalAudit {
  const project = requireProject(state, projectId);
  const report = storageReport(state, projectId, catalogPath);
  const blockers = state.removalBlockers[projectId] ?? {};
  const otherProjectsInCatalog = blockers.otherProjectsInCatalog ?? [];
  const archiveRootsInside = blockers.archiveRootsInside ?? [];
  const originalsInside = blockers.originalsInside ?? 0;
  const unexpectedEntries = blockers.unexpectedEntries ?? [];
  return {
    projectId,
    projectName: project.name,
    catalogPath,
    folderPath,
    catalogBytes: report.catalogBytes,
    cacheBytes: report.cacheBytes,
    assetCount: Object.values(state.assets).filter((asset) => asset.projectId === projectId).length,
    otherProjectsInCatalog,
    archiveRootsInside,
    originalsInside,
    unexpectedEntries,
    folderRemovable:
      otherProjectsInCatalog.length === 0
      && archiveRootsInside.length === 0
      && originalsInside === 0
      && unexpectedEntries.length === 0,
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

  /**
   * Project and cache lifecycle (#16).
   *
   * The harness has no filesystem, so a scenario declares what a project costs
   * (`state.storage`) and these commands account against that declaration. What
   * is modelled faithfully is the part the UI has to get right: categories are
   * separate, a cleared category goes to zero while the others do not move, the
   * catalog is never touched, and the folder-removal verdict comes from the
   * same four blockers the real audit checks.
   */
  project_storage_report: (args, { state }): ProjectStorageReport => {
    const projectId = requireString(args, "projectId");
    requireProject(state, projectId);
    return storageReport(state, projectId, requireString(args, "catalogPath"));
  },

  clear_project_cache: (args, { state }): CacheCleanupReport => {
    const projectId = requireString(args, "projectId");
    requireProject(state, projectId);
    const kinds = Array.isArray(args.kinds) ? (args.kinds as CacheKind[]) : [];
    if (kinds.length === 0) throw new Error("clear_project_cache needs the kinds to clear");
    const stored = state.storage[projectId] ?? {};
    let removedBytes = 0;
    let removedFiles = 0;
    for (const kind of new Set(kinds)) {
      const entry = stored[kind];
      if (!entry) continue;
      removedBytes += entry.bytes;
      removedFiles += entry.fileCount;
      stored[kind] = { bytes: 0, fileCount: 0 };
    }
    state.storage[projectId] = stored;
    return { removedBytes, removedFiles, failures: [] };
  },

  audit_project_removal: (args, { state }): ProjectRemovalAudit =>
    removalAudit(
      state,
      requireString(args, "projectId"),
      requireString(args, "catalogPath"),
      requireString(args, "folderPath"),
    ),

  delete_project: (args, { state }): ProjectRemovalAudit => {
    const projectId = requireString(args, "projectId");
    const audit = removalAudit(
      state,
      projectId,
      requireString(args, "catalogPath"),
      requireString(args, "folderPath"),
    );
    // Rows go; media never does. The harness has no originals to delete and
    // the assertion that matters is that nothing tried.
    state.projects = state.projects.filter((project) => project.id !== projectId);
    for (const assetId of Object.keys(state.assets)) {
      if (state.assets[assetId].projectId === projectId) delete state.assets[assetId];
    }
    state.assetOrder = state.assetOrder.filter((assetId) => state.assets[assetId]);
    delete state.storage[projectId];
    return audit;
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
