/**
 * The surface an import sits on while it waits for a volume (#79).
 *
 * Written as a status panel rather than a dialog, because there is almost
 * never a decision to make here. The common case — lid shut, lid opened, same
 * drives — resolves itself, so the panel's job is to say what is true, keep
 * the committed work visible, and stay out of the way until the volume is
 * back.
 *
 * Two omissions are deliberate and load-bearing:
 *
 * - **No "Check again".** Detection runs on a timer. A button that re-asks a
 *   question already being asked every few seconds does nothing except imply
 *   the waiting is the user's job.
 * - **No clock.** Nothing counts up, counts down, or expires. A pause of four
 *   hours is judged exactly like a pause of four seconds, so showing elapsed
 *   time would only invite the belief that waiting too long forfeits the run.
 */

import {
  correctiveFor,
  pausedHeadline,
  resumeDecision,
  unmetConditionDetail,
  type Corrective,
  type EndpointProbe,
  type PausedImport,
} from "../import/pausedImport";

interface Props {
  paused: PausedImport;
  /** The last look at the endpoint, or null before the first one lands. */
  probe: EndpointProbe | null;
  onCancel(): void;
  onCorrective(corrective: Corrective): void;
}

const CORRECTIVE_LABEL: Record<Corrective, string> = {
  "locate-archive": "Locate archive…",
  "choose-source": "Choose source…",
};

function clips(count: number): string {
  return `${count} clip${count === 1 ? "" : "s"}`;
}

export function PausedImportPanel({ paused, probe, onCancel, onCorrective }: Props) {
  // Before the first probe lands, the endpoint is treated as absent. It is the
  // state the run just paused in, and it is the one that offers nothing to
  // press — so an unproven endpoint can never be implied by a missing answer.
  const decision = resumeDecision(paused, probe ?? { present: false, identityVerified: false });
  const unmet = decision.action === "blocked" ? decision.unmet : [];

  return (
    <div className="import-paused" aria-live="polite" data-endpoint={paused.endpoint}>
      <p className="eyebrow">Waiting for {paused.volumeLabel}</p>
      <h3>{pausedHeadline()}</h3>

      <p className="import-paused-committed">
        {clips(paused.committed)} are already in the project and stay there. Only the remainder is
        left to copy.
      </p>
      {paused.failed > 0 && (
        <p className="import-warning">
          {paused.failed} file{paused.failed === 1 ? "" : "s"} need attention in the import ledger.
        </p>
      )}

      <ul className="import-paused-conditions">
        {unmet.map((condition) => {
          const corrective = correctiveFor(paused, condition);
          return (
            <li key={condition} data-condition={condition}>
              <span>{unmetConditionDetail(paused, condition)}</span>
              {corrective && (
                <button className="button secondary" onClick={() => onCorrective(corrective)}>
                  {CORRECTIVE_LABEL[corrective]}
                </button>
              )}
            </li>
          );
        })}
      </ul>

      <p className="import-paused-note">
        This continues on its own as soon as {paused.volumeLabel} is back and can prove it is the
        same one. Nothing to press.
      </p>

      <details className="import-paused-details">
        <summary>What this import was doing</summary>
        <ul>
          <li>Source: {paused.session.sourcePath}</li>
          <li>
            {paused.session.verifyCopies ? "Verifying copies" : "Copying without verification"}
          </li>
          <li>{paused.session.makeProxies ? "Building proxies" : "No proxies"}</li>
          {paused.session.cameraOverride && <li>Camera: {paused.session.cameraOverride}</li>}
        </ul>
      </details>

      <div className="import-paused-actions">
        <button className="button secondary" onClick={onCancel}>
          Cancel import
        </button>
      </div>
    </div>
  );
}
