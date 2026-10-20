import type { AnalysisReadiness } from "../types";
import { formatCost } from "../review/format";

/**
 * The analyze-project dialog (spec §5). It states the plan, projects the cost,
 * takes a per-run budget and a re-analyze choice, and blocks Start when the
 * models or the API key are missing. It owns none of that state — the shell
 * gates it on `readiness` and drives the run — so this only renders and reports.
 */
export function AnalysisDialog({
  projectName,
  readiness,
  budget,
  forceReanalyze,
  onBudgetChange,
  onForceReanalyzeChange,
  onOpenSettings,
  onCancel,
  onStart,
}: {
  projectName: string;
  readiness: AnalysisReadiness;
  budget: string;
  forceReanalyze: boolean;
  onBudgetChange(value: string): void;
  onForceReanalyzeChange(value: boolean): void;
  onOpenSettings(): void;
  onCancel(): void;
  onStart(): void;
}) {
  return (
    <div className="modal-backdrop" role="presentation">
      <section className="modal analysis-modal" role="dialog" aria-modal="true" aria-labelledby="analysis-title">
        <span className="eyebrow">Analyze project</span>
        <h2 id="analysis-title">{projectName}</h2>

        {(readiness.unverifiedVideoClips > 0 || readiness.quarantinedVideoClips > 0 || readiness.unavailableVideoClips > 0) && (
          <p className="blocking-note" role="status">
            Analysis includes {readiness.totalClips} usable video clips. {readiness.reachableOriginals} original files are reachable; {readiness.unverifiedVideoClips} clips need validation, {readiness.quarantinedVideoClips} are quarantined, and {readiness.unavailableVideoClips} have neither source bytes nor a proxy available. Use <code>clips validate-video --asset &lt;id&gt;</code> after checking each source.
          </p>
        )}

        <ol className="stage-plan">
          <li>
            <strong>{readiness.missingProxies}</strong>
            <span>proxies to make</span>
            <small>Local. Both later steps read the proxy, so this runs first.</small>
          </li>
          <li>
            <strong>{readiness.missingTranscripts}</strong>
            <span>clips to transcribe</span>
            <small>Local and offline. No API key, no cost.</small>
          </li>
          <li>
            <strong>{readiness.missingAnalyses}</strong>
            <span>clips to analyze</span>
            <small>Cloud vision. This is the step that spends money.</small>
          </li>
        </ol>

        <div className="cost-projection">
          <div>
            <span>Projected cost</span>
            <strong>{formatCost(readiness.estimatedCostUsd)}</strong>
          </div>
          <p>
            {readiness.costBasis === "observed"
              ? `Based on $${readiness.observedCostPerClipUsd.toFixed(6)} per clip already analyzed in this project.`
              : `No clips analyzed here yet, so this uses the reference measurement of $${readiness.observedCostPerClipUsd.toFixed(6)} per clip. The first run replaces it with your own.`}
            {readiness.lastRun && readiness.lastRun.status !== "running"
              ? ` Last run: ${readiness.lastRun.clipsAnalyzed} clips, ${formatCost(readiness.lastRun.costUsd)}.`
              : ""}
          </p>
        </div>

        <label className="field-label">Stop this run at
          <input
            type="number"
            min="0"
            step="0.25"
            value={budget}
            onChange={(event) => onBudgetChange(event.target.value)}
            aria-describedby="budget-note"
          />
        </label>
        <p id="budget-note" className="section-note">
          US dollars. The run stops once returned provider usage reaches this, keeping whatever it finished.
        </p>

        <label className="checkbox-row">
          <input type="checkbox" checked={forceReanalyze} onChange={(event) => onForceReanalyzeChange(event.target.checked)} />
          <span>Re-analyze clips that already have a description. Your edits to titles and descriptions will be overwritten; notes and your own labels are never touched.</span>
        </label>

        {!readiness.modelsReady && (
          <p className="blocking-note">
            Speech models are not installed, so transcription would fail for every clip.
            {" "}
            <button className="text-button" onClick={onOpenSettings}>Install them in Settings</button>
          </p>
        )}
        {!readiness.apiKeyPresent && (
          <p className="blocking-note">
            No OpenRouter key, so clip analysis cannot run.
            {" "}
            <button className="text-button" onClick={onOpenSettings}>Add one in Settings</button>
          </p>
        )}

        <div className="modal-actions">
          <button className="button ghost" onClick={onCancel}>Cancel</button>
          <button
            className="button primary"
            disabled={!readiness.modelsReady || !readiness.apiKeyPresent || readiness.totalClips === 0}
            onClick={onStart}
          >
            Start
          </button>
        </div>
      </section>
    </div>
  );
}
