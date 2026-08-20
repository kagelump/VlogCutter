import type { ProjectLibrary, ProjectLocation } from "../types";

/**
 * The project library / new-project modal (spec §2). Lists the projects found
 * in the projects folder, opens one, and creates a new one. Desktop-only
 * affordances (change the folder, open an out-of-folder catalog) are gated on
 * `isDesktop`, matching the shell. All state and side effects stay in `App`.
 */
export function NewProjectModal({
  library,
  isDesktop,
  busy,
  newProjectName,
  canCancel,
  onChangeRoot,
  onOpenProject,
  onDeleteProject,
  onNewProjectNameChange,
  onOpenCatalog,
  onCancel,
  onCreate,
}: {
  library: ProjectLibrary;
  isDesktop: boolean;
  busy: boolean;
  newProjectName: string;
  canCancel: boolean;
  onChangeRoot(): void;
  onOpenProject(location: ProjectLocation): void;
  /**
   * Delete a project from the library (#16). Present only on desktop, where
   * there is a folder to remove; the browser preview has nothing to delete.
   */
  onDeleteProject?(location: ProjectLocation): void;
  onNewProjectNameChange(value: string): void;
  onOpenCatalog(): void;
  onCancel(): void;
  onCreate(): void;
}) {
  return (
    <div className="modal-backdrop" role="presentation">
      <section className="modal" role="dialog" aria-modal="true" aria-labelledby="new-project-title">
        <span className="eyebrow">Trip projects</span>
        <h2 id="new-project-title">Open or create a working set</h2>
        <div className="project-library-heading">
          <span className="section-label">Projects folder</span>
          {isDesktop && <button className="text-button" disabled={busy} onClick={onChangeRoot}>Change…</button>}
        </div>
        <div className="project-root mono" title={library.rootPath}>{library.rootPath || "Loading…"}</div>
        {library.projects.length > 0 ? (
          <div className="project-library-list">
            {library.projects.map((location) => (
              /* A row, not a button: the row opens the project and carries a
                 separate destructive action beside it. Abandoned and test
                 projects accumulate exactly here, so this is where removing
                 one belongs — not behind a hunt through Settings (#16). */
              <div className="project-library-row" key={`${location.catalogPath}:${location.project.id}`}>
                <button
                  className="project-library-item"
                  disabled={busy}
                  /* Named explicitly so it pairs with the Delete beside it:
                     two controls about the same project need two names that
                     say which is which, not one that happens to read first. */
                  aria-label={`Open ${location.project.name}`}
                  onClick={() => onOpenProject(location)}
                >
                  <strong>{location.project.name}</strong>
                  <span>{location.project.assetCount} clips · {location.project.labeledCount} labeled</span>
                </button>
                {onDeleteProject && (
                  <button
                    type="button"
                    className="project-library-delete"
                    disabled={busy}
                    aria-label={`Delete ${location.project.name}`}
                    title={`Delete ${location.project.name}`}
                    onClick={() => onDeleteProject(location)}
                  >
                    Delete
                  </button>
                )}
              </div>
            ))}
          </div>
        ) : (
          <p className="project-library-empty">No VlogCutter projects found in this folder.</p>
        )}
        <div className="modal-divider"><span>New project</span></div>
        <p>Originals stay where they are. The catalog, proxies, and thumbnails will live in this project’s folder.</p>
        <input
          autoFocus
          value={newProjectName}
          onChange={(event) => onNewProjectNameChange(event.target.value)}
          onKeyDown={(event) => event.key === "Enter" && onCreate()}
          placeholder="e.g. Hokkaido winter 2026"
        />
        <div className="modal-actions">
          {isDesktop && (
            <button className="button secondary" disabled={busy} onClick={onOpenCatalog}>
              Open existing project…
            </button>
          )}
          {canCancel && <button className="button secondary" onClick={onCancel}>Cancel</button>}
          <button className="button primary" disabled={busy || !newProjectName.trim()} onClick={onCreate}>Create project</button>
        </div>
      </section>
    </div>
  );
}
