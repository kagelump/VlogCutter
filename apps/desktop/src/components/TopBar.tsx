import { useEffect, useRef, useState } from "react";
import type { AnalysisStage, ImportStage, Project, SyncStatus } from "../types";
import type { Mode } from "../review/presentation";
import { analysisStageLabels, formatCost, formatEta, syncStateLabel } from "../review/format";

/** A long-running background job, as the topbar reports it. */
export interface OperationProgress {
  kind: "proxy" | "thumbnail" | "analysis" | "import";
  completed: number;
  total: number;
  completedWorkUnits: number;
  totalWorkUnits: number;
  workerCount: number;
  estimatedRemainingSeconds: number | null;
  /** Analysis only: which stage of the chain is running, and what it has spent. */
  stage?: AnalysisStage;
  stageIndex?: number;
  stageCount?: number;
  costUsd?: number;
  /** Import only: copying, metadata, or proxy generation. */
  importStage?: ImportStage;
  currentFile?: string;
}

export interface TopBarProps {
  projects: Project[];
  projectId: string;
  mode: Mode;
  /** The browser build has no catalog and no server pairing. */
  isDesktop: boolean;
  busy: boolean;
  canPairWithServer: boolean;
  syncStatus: SyncStatus | null;

  /** The operation slot: one background media/import operation at a time. */
  operationProgress: OperationProgress | null;
  error: string;
  message: string;

  /** Outstanding work, which is what decides whether each control exists. */
  missingProxyCount: number;
  missingThumbnailCount: number;
  missingAnalysisCount: number;
  mediaMenuOpen: boolean;
  hasLastImport: boolean;

  /** The completion hairline. */
  labeledCount: number;
  activeCount: number;

  onProjectChange(id: string): void;
  onModeChange(mode: Mode): void;
  onOpenCatalog(): void;
  onOpenSettings(): void;
  onOpenCreateProject(): void;
  onOpenShortcuts(): void;
  onPairWithServer(): void;
  onShowSyncDiagnostics(): void;
  onCancelAnalysis(): void;
  onCancelImport(): void;
  onDismissError(): void;
  onMakeProxies(): void;
  onToggleMediaMenu(): void;
  onMakeThumbnails(): void;
  onOpenAnalysis(): void;
  onImportFolder(): void;
  onReviewLastImport(): void;
}

const modes: Array<[Mode, string, string]> = [
  ["review", "Review", "Meta+1"],
  ["chronology", "Chronology", "Meta+2"],
  ["editPlan", "Edit plan", "Meta+3"],
];

/**
 * The topbar (spec §3.2).
 *
 * It absorbed the 28px status line, which was reserved whether or not it had
 * anything to say. What replaces it is the operation slot: empty at idle, and
 * that emptiness is deliberate — it is the only calm region on screen, and a
 * row that is always occupied stops being read.
 */
export function TopBar({
  projects,
  projectId,
  mode,
  isDesktop,
  busy,
  canPairWithServer,
  syncStatus,
  operationProgress,
  error,
  message,
  missingProxyCount,
  missingThumbnailCount,
  missingAnalysisCount,
  mediaMenuOpen,
  hasLastImport,
  labeledCount,
  activeCount,
  onProjectChange,
  onModeChange,
  onOpenCatalog,
  onOpenSettings,
  onOpenCreateProject,
  onOpenShortcuts,
  onPairWithServer,
  onShowSyncDiagnostics,
  onCancelAnalysis,
  onCancelImport,
  onDismissError,
  onMakeProxies,
  onToggleMediaMenu,
  onMakeThumbnails,
  onOpenAnalysis,
  onImportFolder,
  onReviewLastImport,
}: TopBarProps) {
  const [projectMenuOpen, setProjectMenuOpen] = useState(false);
  const projectMenuRef = useRef<HTMLDivElement>(null);
  const runProjectAction = (action: () => void) => {
    setProjectMenuOpen(false);
    action();
  };
  useEffect(() => {
    if (!projectMenuOpen) return;
    const closeOutside = (event: PointerEvent) => {
      if (!projectMenuRef.current?.contains(event.target as Node)) setProjectMenuOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setProjectMenuOpen(false);
    };
    document.addEventListener("pointerdown", closeOutside);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOutside);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [projectMenuOpen]);
  return (
    <header className="topbar">
      <div className="wordmark" aria-label="RCD VlogCutter">
        <span className="wordmark-mark">RCD</span>
        <span>VlogCutter</span>
      </div>
      <label className="project-switcher">
        <span className="sr-only">Current project</span>
        <select value={projectId} onChange={(event) => onProjectChange(event.target.value)}>
          {projects.map((item) => (
            <option value={item.id} key={item.id}>{item.name}</option>
          ))}
        </select>
      </label>
      {/* Visible rather than tucked into a menu: three modes is few enough
          to show, and hiding them makes them read as settings. */}
      <nav className="mode-switcher" aria-label="View mode">
        {modes.map(([value, label, shortcut]) => (
          <button
            key={value}
            className={mode === value ? "active" : ""}
            aria-pressed={mode === value}
            aria-keyshortcuts={shortcut}
            disabled={!projectId && value !== "review"}
            onClick={() => onModeChange(value)}
          >{label}</button>
        ))}
      </nav>
      {isDesktop && <button className="text-button desktop-lifecycle-action" disabled={busy} onClick={onOpenCatalog} aria-keyshortcuts="Meta+O">Open catalog</button>}
      <button className="text-button desktop-lifecycle-action" onClick={onOpenSettings} aria-keyshortcuts="Meta+,">Settings <kbd>⌘,</kbd></button>
      <button className="text-button desktop-lifecycle-action" onClick={onOpenCreateProject} aria-keyshortcuts="Meta+N">New project</button>
      {isDesktop && canPairWithServer && (
        <button className="text-button desktop-lifecycle-action" disabled={busy} onClick={onPairWithServer}>
          Pair with server
        </button>
      )}
      <div className="topbar-overflow" ref={projectMenuRef}>
        <button type="button" aria-label="Project actions" aria-expanded={projectMenuOpen} onClick={() => setProjectMenuOpen((open) => !open)}>•••</button>
        {projectMenuOpen && (
          <div>
            {isDesktop && <button disabled={busy} onClick={() => runProjectAction(onOpenCatalog)}>Open catalog</button>}
            <button onClick={() => runProjectAction(onOpenSettings)}>Settings</button>
            <button onClick={() => runProjectAction(onOpenCreateProject)}>New project</button>
            {isDesktop && canPairWithServer && <button disabled={busy} onClick={() => runProjectAction(onPairWithServer)}>Pair with server</button>}
          </div>
        )}
      </div>
      {syncStatus && syncStatus.state !== "unpaired" && (
        <button
          className={`sync-indicator sync-${syncStatus.state}`}
          onClick={onShowSyncDiagnostics}
          title={`Sync: ${syncStatus.state}${syncStatus.lastError ? ` — ${syncStatus.lastError}` : ""}`}
        >
          <span className={`sync-dot sync-dot-${syncStatus.state}`} />
          <span className="sync-label">{syncStateLabel(syncStatus.state)}</span>
        </button>
      )}
      {/* Empty at idle, and that emptiness is the point: this is the only
          calm region on screen. It is the whole replacement for the 28px
          status line that used to be reserved whether or not it had
          anything to say. */}
      <div className="operation-slot" data-testid="operation-slot" aria-live="polite">
        {operationProgress ? (
          <>
            <span>
              {operationProgress.kind === "analysis" && operationProgress.stage
                ? `${analysisStageLabels[operationProgress.stage]} · step ${(operationProgress.stageIndex ?? 0) + 1} of ${operationProgress.stageCount ?? 3}`
                : operationProgress.kind === "import"
                  ? operationProgress.importStage === "copying"
                    ? "Copying and verifying originals"
                    : operationProgress.importStage === "reading"
                      ? "Reading clip metadata"
                      : operationProgress.importStage === "proxies"
                        ? "Making import proxies"
                        : "Importing media"
                : operationProgress.kind === "thumbnail"
                  ? "Generating thumbnails"
                  : "Generating proxies"}
            </span>
            <div
              className="progress-track"
              role="progressbar"
              aria-valuemin={0}
              aria-valuemax={operationProgress.totalWorkUnits || operationProgress.total}
              aria-valuenow={operationProgress.totalWorkUnits ? operationProgress.completedWorkUnits : operationProgress.completed}
            >
              <span style={{ width: `${operationProgress.totalWorkUnits
                ? (operationProgress.completedWorkUnits / operationProgress.totalWorkUnits) * 100
                : operationProgress.total
                  ? (operationProgress.completed / operationProgress.total) * 100
                  : 0}%` }} />
            </div>
            <output title={operationProgress.currentFile}>{operationProgress.completed}/{operationProgress.total}</output>
            <span>
              {formatEta(operationProgress.estimatedRemainingSeconds)} · {operationProgress.workerCount} worker{operationProgress.workerCount === 1 ? "" : "s"}
              {operationProgress.kind === "analysis" ? ` · ${formatCost(operationProgress.costUsd ?? 0)} spent` : ""}
            </span>
            {operationProgress.kind === "analysis" && (
              <button className="button ghost progress-cancel" onClick={onCancelAnalysis}>Cancel</button>
            )}
            {operationProgress.kind === "import" && (
              <button className="button ghost progress-cancel" onClick={onCancelImport}>Stop</button>
            )}
          </>
        ) : error ? (
          <span className="error-banner" role="alert">
            {error}
            <button className="banner-dismiss" onClick={onDismissError} aria-label="Dismiss error">×</button>
          </span>
        ) : message ? (
          <span className="toast">{message}</span>
        ) : null}
      </div>
      <div className="topbar-spacer" />
      <div className="media-split">
        {/* Not rendered when there is nothing to make. "Proxies ready" is a
            disabled button reporting the absence of work, and it was the
            source of the mismatched-outline complaint. The count is state
            worth seeing, so the button returns when work is outstanding. */}
        {missingProxyCount > 0 && (
          <button className="button secondary media-split-main" disabled={busy} onClick={onMakeProxies} aria-keyshortcuts="Shift+P">
            Make {missingProxyCount} proxies
          </button>
        )}
        <button
          className="button secondary media-split-toggle"
          disabled={busy || !projectId}
          onClick={onToggleMediaMenu}
          aria-label="More media generation actions"
          aria-expanded={mediaMenuOpen}
        >▾</button>
        {mediaMenuOpen && (
          <div className="media-menu">
            {hasLastImport && (
              <button onClick={onReviewLastImport}>
                <span>Review last import</span>
                <small>File-by-file results from this session</small>
              </button>
            )}
            <button disabled={busy || missingThumbnailCount === 0} onClick={onMakeThumbnails} aria-keyshortcuts="Shift+G">
              <span>{missingThumbnailCount ? `Generate ${missingThumbnailCount} thumbnails` : "Thumbnails ready"}</span>
              <kbd>⇧G</kbd>
              <small>Quality 1 · frame at 20%</small>
            </button>
            <button disabled={busy || !projectId} onClick={onOpenAnalysis} aria-keyshortcuts="A">
              <span>{missingAnalysisCount ? `Analyze ${missingAnalysisCount} clips` : "Analysis up to date"}</span>
              <kbd>A</kbd>
              <small>Proxies, speech, then cloud vision</small>
            </button>
          </div>
        )}
      </div>
      <button className="button primary" disabled={busy || !projectId} onClick={onImportFolder} aria-keyshortcuts="M">
        Add media <kbd>M</kbd>
      </button>
      <button className="shortcut-help-button" onClick={onOpenShortcuts} aria-label="Keyboard shortcuts" aria-keyshortcuts="?">?</button>
      {/* Ambient progress for 2px, replacing a 180px readout that said the
          same thing in words. */}
      <div className="completion-hairline" aria-hidden="true">
        <span style={{ width: `${activeCount ? (labeledCount / activeCount) * 100 : 0}%` }} />
      </div>
    </header>
  );
}
