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
              <button
                className="project-library-item"
                disabled={busy}
                key={`${location.catalogPath}:${location.project.id}`}
                onClick={() => onOpenProject(location)}
              >
                <strong>{location.project.name}</strong>
                <span>{location.project.assetCount} clips · {location.project.labeledCount} labeled</span>
              </button>
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
