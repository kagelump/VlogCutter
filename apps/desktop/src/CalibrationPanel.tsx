// The calibration surface (D01-T015 read-only, D01-T016 editing) and the
// diagnostics surface (D01-T027 consumer).
//
// Two rules shape this component:
//
//   1. It displays what the solver decided. It never sorts, re-derives a time,
//      or computes a confidence of its own — the desktop and the CLI must not
//      be able to disagree about order.
//   2. No chronology change is applied without explicit user confirmation, and
//      the confirmation shows the *impact* first. Every write here is preceded
//      by a preview and a queue-position capture.

import { useCallback, useEffect, useMemo, useState } from "react";
import { chronologyApi } from "./chronologyApi";
import type {
  ChronologyExplanation,
  ChronologyOverview,
  ClockEpoch,
  Contradiction,
  Diagnostic,
  DiagnosticsView,
} from "./chronologyTypes";

interface Props {
  projectId: string;
  /** The clip the review queue is standing on, so a change can resume there. */
  currentAssetId: string | null;
  /** Called with the asset to resume on after a change reorders the queue. */
  onQueueResumed?: (assetId: string) => void;
}

const emptyDiagnostics: DiagnosticsView = {
  diagnostics: [],
  counts: { errors: 0, warnings: 0, infos: 0 },
};

function formatOffset(seconds: number): string {
  const sign = seconds < 0 ? "−" : "+";
  const total = Math.abs(seconds);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const rest = total % 60;
  if (hours > 0) return `${sign}${hours}h ${minutes}m`;
  if (minutes > 0) return `${sign}${minutes}m ${rest}s`;
  return `${sign}${rest}s`;
}

function formatRange(epoch: ClockEpoch): string {
  return epoch.endCounter === null
    ? `${epoch.startCounter} and up`
    : `${epoch.startCounter}–${epoch.endCounter}`;
}

export function CalibrationPanel({ projectId, currentAssetId, onQueueResumed }: Props) {
  const [overview, setOverview] = useState<ChronologyOverview | null>(null);
  const [diagnostics, setDiagnostics] = useState<DiagnosticsView>(emptyDiagnostics);
  const [explanation, setExplanation] = useState<ChronologyExplanation | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // A pending epoch and the contradictions it would create. The user confirms
  // against this, never against a bare form.
  const [pending, setPending] = useState<{ epoch: ClockEpoch; impact: Contradiction[] } | null>(
    null,
  );
  const [draft, setDraft] = useState({
    sequenceId: "",
    startCounter: "0",
    endCounter: "",
    offsetMinutes: "0",
    label: "",
  });

  const refresh = useCallback(async () => {
    setError(null);
    try {
      const [next, found] = await Promise.all([
        chronologyApi.overview(projectId),
        chronologyApi.diagnostics(projectId),
      ]);
      setOverview(next);
      setDiagnostics(found);
    } catch (cause) {
      setError(String(cause));
    }
  }, [projectId]);

  useEffect(() => {
    void refresh();
    // Detecting offline volumes on open is what makes an unmounted card a
    // visible condition instead of a mystery about missing thumbnails.
    void chronologyApi.detectOfflineVolumes(projectId).then((found) => {
      if (found.length > 0) void refresh();
    });
  }, [projectId, refresh]);

  useEffect(() => {
    if (!currentAssetId) {
      setExplanation(null);
      return;
    }
    void chronologyApi.explain(currentAssetId).then(setExplanation).catch(() => setExplanation(null));
  }, [currentAssetId, overview]);

  const buildEpoch = useCallback((): ClockEpoch => {
    const end = draft.endCounter.trim();
    return {
      id: "",
      captureSequenceId: draft.sequenceId,
      startCounter: Number(draft.startCounter) || 0,
      endCounter: end === "" ? null : Number(end),
      offsetSeconds: Math.round((Number(draft.offsetMinutes) || 0) * 60),
      confidence: 1,
      evidence: ["user_set"],
      label: draft.label,
    };
  }, [draft]);

  const previewChange = useCallback(async () => {
    setError(null);
    setBusy(true);
    try {
      const epoch = buildEpoch();
      const impact = await chronologyApi.previewClockEpoch(projectId, epoch);
      setPending({ epoch, impact });
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }, [buildEpoch, projectId]);

  /**
   * Applies the previewed change.
   *
   * The queue position is captured *before* the write and restored after, so a
   * correction that reorders hundreds of clips does not lose the user's place.
   */
  const applyChange = useCallback(async () => {
    if (!pending) return;
    setError(null);
    setBusy(true);
    try {
      const position = currentAssetId
        ? await chronologyApi.captureQueuePosition(projectId, currentAssetId)
        : null;
      await chronologyApi.setClockEpoch(projectId, pending.epoch);
      setPending(null);
      await refresh();
      if (position) {
        const resumed = await chronologyApi.restoreQueuePosition(projectId, position);
        if (resumed && onQueueResumed) onQueueResumed(resumed);
      }
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }, [currentAssetId, onQueueResumed, pending, projectId, refresh]);

  const clearEpoch = useCallback(
    async (epochId: string) => {
      setBusy(true);
      try {
        await chronologyApi.clearClockEpoch(projectId, epochId);
        await refresh();
      } catch (cause) {
        setError(String(cause));
      } finally {
        setBusy(false);
      }
    },
    [projectId, refresh],
  );

  const applySplit = useCallback(
    async (sequenceId: string, atCounter: number) => {
      setBusy(true);
      try {
        await chronologyApi.splitCaptureSequence(
          projectId,
          sequenceId,
          atCounter,
          `Card from ${atCounter}`,
        );
        await refresh();
      } catch (cause) {
        setError(String(cause));
      } finally {
        setBusy(false);
      }
    },
    [projectId, refresh],
  );

  const acknowledge = useCallback(
    async (diagnostic: Diagnostic) => {
      await chronologyApi.acknowledgeDiagnostic(projectId, diagnostic.id);
      await refresh();
    },
    [projectId, refresh],
  );

  const hardContradictions = useMemo(
    () => overview?.contradictions.filter((found) => found.strength === "hard") ?? [],
    [overview],
  );

  if (!overview) {
    return (
      <section className="calibration-panel" aria-busy="true">
        <p>{error ?? "Loading chronology…"}</p>
      </section>
    );
  }

  return (
    <section className="calibration-panel" aria-label="Chronology calibration">
      {error && (
        <p className="calibration-error" role="alert">
          {error}
        </p>
      )}

      {/* Hard contradictions block apply and name the conflicting evidence. */}
      {hardContradictions.length > 0 && (
        <div className="calibration-contradictions" role="alert">
          <h3>{hardContradictions.length} contradiction(s) block changes</h3>
          <ul>
            {hardContradictions.map((found) => (
              <li key={`${found.kind}:${found.assetIds.join(",")}`}>
                {found.message}
                <span className="calibration-evidence">
                  {found.evidence.map((item) => item.detail).join("; ")}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {/* One surface for every condition the user must know about. */}
      {diagnostics.diagnostics.length > 0 && (
        <div className="calibration-diagnostics">
          <h3>
            Needs attention — {diagnostics.counts.errors} error(s),{" "}
            {diagnostics.counts.warnings} warning(s), {diagnostics.counts.infos} note(s)
          </h3>
          <ul>
            {diagnostics.diagnostics.map((found) => (
              <li key={found.id} data-severity={found.severity} data-kind={found.kind}>
                <span>{found.message}</span>
                {found.occurrenceCount > 1 && <em> ({found.occurrenceCount} times)</em>}
                <button type="button" onClick={() => void acknowledge(found)}>
                  Acknowledge
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}

      <div className="calibration-summary">
        <span>{overview.assets.length} clips</span>
        <span>{overview.sequences.length} capture sequences</span>
        <span>{overview.epochs.length} clock corrections</span>
        <span>{overview.phases.length} trip phases</span>
        {overview.unplacedAssets > 0 && (
          <span className="calibration-unplaced">
            {overview.unplacedAssets} clip(s) have no time
          </span>
        )}
      </div>

      <h3>Capture sequences</h3>
      <ul className="calibration-sequences">
        {overview.sequences.map((sequence) => (
          <li key={sequence.id}>
            <strong>{sequence.label || sequence.cameraKey}</strong>
            {sequence.volumeHint && <span> — {sequence.volumeHint}</span>}
          </li>
        ))}
      </ul>

      {/* Proposals, never applied automatically. */}
      {overview.proposedSplits.length > 0 && (
        <div className="calibration-proposals">
          <h3>Suggested card splits</h3>
          <ul>
            {overview.proposedSplits.map((proposal) => (
              <li key={`${proposal.captureSequenceId}:${proposal.atCounter}`}>
                <span>{proposal.reason}</span>
                <em> — {proposal.affectedAssetIds.length} clips would move</em>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() =>
                    void applySplit(proposal.captureSequenceId, proposal.atCounter)
                  }
                >
                  Split here
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}

      <h3>Clock corrections</h3>
      <ul className="calibration-epochs">
        {overview.epochs.map((epoch) => (
          <li key={epoch.id}>
            <span>
              {epoch.label || "Correction"} — {formatOffset(epoch.offsetSeconds)} over clips{" "}
              {formatRange(epoch)}
            </span>
            <button type="button" disabled={busy} onClick={() => void clearEpoch(epoch.id)}>
              Remove
            </button>
          </li>
        ))}
        {overview.epochs.length === 0 && <li>No clock corrections yet.</li>}
      </ul>

      <form
        className="calibration-editor"
        onSubmit={(event) => {
          event.preventDefault();
          void previewChange();
        }}
      >
        <h3>Correct a camera clock</h3>
        <label>
          Capture sequence
          <select
            value={draft.sequenceId}
            onChange={(event) => setDraft({ ...draft, sequenceId: event.target.value })}
            required
          >
            <option value="">Choose…</option>
            {overview.sequences.map((sequence) => (
              <option key={sequence.id} value={sequence.id}>
                {sequence.label || sequence.cameraKey}
              </option>
            ))}
          </select>
        </label>
        <label>
          From clip number
          <input
            type="number"
            min={0}
            value={draft.startCounter}
            onChange={(event) => setDraft({ ...draft, startCounter: event.target.value })}
          />
        </label>
        <label>
          To clip number (blank for all later clips)
          <input
            type="number"
            min={0}
            value={draft.endCounter}
            onChange={(event) => setDraft({ ...draft, endCounter: event.target.value })}
          />
        </label>
        <label>
          Shift by (minutes)
          <input
            type="number"
            value={draft.offsetMinutes}
            onChange={(event) => setDraft({ ...draft, offsetMinutes: event.target.value })}
          />
        </label>
        <label>
          Why
          <input
            type="text"
            value={draft.label}
            onChange={(event) => setDraft({ ...draft, label: event.target.value })}
            placeholder="Camera clock never left home time"
          />
        </label>
        <button type="submit" disabled={busy || !draft.sequenceId}>
          Preview impact
        </button>
      </form>

      {/* The confirmation step. The user sees the consequence, then decides. */}
      {pending && (
        <div className="calibration-confirm" role="dialog" aria-label="Confirm chronology change">
          <h3>Apply this correction?</h3>
          <p>
            {formatOffset(pending.epoch.offsetSeconds)} over clips {formatRange(pending.epoch)}.
          </p>
          {pending.impact.length > 0 ? (
            <div className="calibration-impact" role="alert">
              <p>This would create {pending.impact.length} contradiction(s):</p>
              <ul>
                {pending.impact.map((found) => (
                  <li key={`${found.kind}:${found.assetIds.join(",")}`}>{found.message}</li>
                ))}
              </ul>
            </div>
          ) : (
            <p className="calibration-impact">No contradictions.</p>
          )}
          <button
            type="button"
            disabled={busy || pending.impact.some((found) => found.strength === "hard")}
            onClick={() => void applyChange()}
          >
            Apply
          </button>
          <button type="button" disabled={busy} onClick={() => setPending(null)}>
            Cancel
          </button>
        </div>
      )}

      {/* Why is this clip here? Named by identity, so the user can act on it. */}
      {explanation && (
        <div className="calibration-explanation">
          <h3>Why this clip is here</h3>
          <p>
            Camera said {explanation.rawTime ?? "nothing"}; placed at{" "}
            {explanation.tripTime ?? "no time"}
            {explanation.displayTime &&
              ` (${explanation.displayTime.primary} ${explanation.displayTime.timezone})`}
            .
          </p>
          <p>Confidence {Math.round(explanation.confidence * 100)}%.</p>
          <ul>
            {explanation.appliedCorrections.map((correction, index) => (
              <li key={`${correction.type}:${index}`}>
                {correction.type === "captureSequence" && `Grouped with ${correction.label}`}
                {correction.type === "clockEpoch" &&
                  `Clock shifted ${formatOffset(correction.offsetSeconds)}${
                    correction.label ? ` — ${correction.label}` : ""
                  }`}
                {correction.type === "clipException" &&
                  `Placed by hand at ${correction.tripTime}${
                    correction.reason ? ` — ${correction.reason}` : ""
                  }`}
                {correction.type === "tripPhase" &&
                  `Shown in ${correction.timezone} from ${correction.label}`}
                {correction.type === "eventOrder" &&
                  `${correction.strength === "hard" ? "Must" : "Should"} keep its place relative to another clip`}
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
