import { useEffect, useRef, useState } from "react";
import type { CacheCategory, ProjectRemovalAudit } from "../types";
import { formatBytes } from "../review/summaries";
import { CACHE_KIND_LABELS } from "./storageLabels";

/**
 * The destructive confirmation for removing a project (#16).
 *
 * Three jobs, in this order:
 *
 * 1. **Draw the boundary.** The single most important sentence in this dialog
 *    is that originals and archive copies are not being deleted. It is stated
 *    plainly, above the fold, not buried in a caveat.
 * 2. **Say what is actually lost.** Annotations, chronology, and import history
 *    are the user's own work and cannot be regenerated. Caches can. The two are
 *    listed apart, because "12 GB" sounds alarming until you know 11.9 GB of it
 *    is proxies the app can remake.
 * 3. **Say what will be left behind, and why.** When the folder holds an
 *    archive root, an original, another project, or a file the user put there,
 *    the folder survives — and the reason is named rather than implied.
 *
 * Typing the project's name is the gate. A destructive action reached by one
 * click from a list is exactly the action that gets taken by accident.
 */
export function DeleteProjectDialog({
  audit,
  busy,
  error,
  onCancel,
  onConfirm,
}: {
  audit: ProjectRemovalAudit;
  busy: boolean;
  error: string;
  onCancel(): void;
  onConfirm(): void;
}) {
  const [typedName, setTypedName] = useState("");
  const dialogRef = useRef<HTMLElement>(null);
  const confirmed = typedName.trim() === audit.projectName.trim();

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape" && !busy) onCancel();
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [busy, onCancel]);

  const blockers = [
    audit.archiveRootsInside.length > 0
      ? `it holds ${audit.archiveRootsInside.length === 1 ? "the archive root" : "archive roots"} ${audit.archiveRootsInside.map((name) => `“${name}”`).join(", ")}`
      : "",
    audit.originalsInside > 0
      ? `${audit.originalsInside} original${audit.originalsInside === 1 ? "" : "s"} live inside it`
      : "",
    audit.otherProjectsInCatalog.length > 0
      ? `${audit.otherProjectsInCatalog.map((name) => `“${name}”`).join(", ")} share${audit.otherProjectsInCatalog.length === 1 ? "s" : ""} the same catalog`
      : "",
    audit.unexpectedEntries.length > 0
      ? `it contains ${audit.unexpectedEntries.map((name) => `“${name}”`).join(", ")}, which VlogCutter did not put there`
      : "",
  ].filter(Boolean);

  return (
    <div className="modal-backdrop context-modal-backdrop" role="presentation">
      <section
        ref={dialogRef}
        className="modal context-modal delete-project-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="delete-project-title"
      >
        <header className="context-modal-header">
          <div>
            <span className="eyebrow">Delete project</span>
            <h2 id="delete-project-title">Delete “{audit.projectName}”?</h2>
          </div>
          <button
            type="button"
            className="context-modal-close"
            aria-label="Close delete project dialog"
            disabled={busy}
            onClick={onCancel}
          >×</button>
        </header>

        <div className="delete-project-body">
          <p className="delete-project-boundary">
            <strong>Your footage is not touched.</strong> This deletes VlogCutter’s own record of
            the project. Originals stay where they are, and any archive copies VlogCutter made
            during import stay exactly where they were put.
          </p>

          <section className="delete-project-section" aria-label="What is lost">
            <h3>Lost for good</h3>
            <ul>
              <li>
                Labels, notes, titles, chronology corrections, and import history for{" "}
                <strong>{audit.assetCount} clip{audit.assetCount === 1 ? "" : "s"}</strong>
                {" — "}
                {formatBytes(audit.catalogBytes)} of catalog. None of this can be regenerated.
              </li>
            </ul>
          </section>

          <section className="delete-project-section" aria-label="What is regenerable">
            <h3>Reclaimed, and remakeable</h3>
            <ul>
              <li>
                {formatBytes(audit.cacheBytes)} of proxies, posters, and analysis caches. VlogCutter
                can make these again from the originals.
              </li>
            </ul>
          </section>

          {audit.folderRemovable ? (
            <p className="delete-project-folder">
              The project folder <code>{audit.folderPath}</code> is removed with it.
            </p>
          ) : (
            <p className="delete-project-folder is-kept" role="status">
              The folder <code>{audit.folderPath}</code> is <strong>kept</strong>, because{" "}
              {blockers.join("; ")}. Only this project’s catalog rows and its own caches are
              removed.
            </p>
          )}

          {error && <p className="context-modal-error" role="alert">{error}</p>}

          <label className="context-form-field delete-project-confirm">
            {/* One line, not three: `.context-form-field` is a grid, so a bare
                `<strong>` in the middle of the sentence would take its own row
                and break the instruction in half. */}
            <span>Type <strong>{audit.projectName}</strong> to confirm</span>
            <input
              autoFocus
              value={typedName}
              onChange={(event) => setTypedName(event.target.value)}
              placeholder={audit.projectName}
              aria-label={`Type ${audit.projectName} to confirm deletion`}
            />
          </label>
        </div>

        <div className="modal-actions">
          <button type="button" className="button" disabled={busy} onClick={onCancel}>Keep project</button>
          <button
            type="button"
            className="button danger"
            disabled={busy || !confirmed}
            onClick={onConfirm}
          >
            {busy ? "Deleting…" : "Delete project"}
          </button>
        </div>
      </section>
    </div>
  );
}

/** One measured cache row, shared by the delete dialog and Settings. */
export function CacheCategoryRow({ category }: { category: CacheCategory }) {
  const label = CACHE_KIND_LABELS[category.kind];
  return (
    <li data-kind={category.kind} data-scope={category.scope}>
      <span className="storage-kind">{label.title}</span>
      <span className="storage-size">{formatBytes(category.bytes)}</span>
      <small>{label.regeneration}</small>
    </li>
  );
}
