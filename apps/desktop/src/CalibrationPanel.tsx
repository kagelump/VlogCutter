// The chronology desk: a visual, solver-faithful correction workflow.
//
// Every preview comes from Rust. The UI selects identities and expresses intent;
// it never derives times, counters, or order. Every write is explicitly
// confirmed after the preview, captures Review's queue position first, and
// restores it afterwards.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "./api";
import { chronologyApi } from "./chronologyApi";
import { RIBBON_ZOOM_DEFAULT, RIBBON_ZOOM_MAX, RIBBON_ZOOM_MIN, RIBBON_ZOOM_STEP } from "./review/layout";
import type { Asset, MediaOrigin } from "./types";
import type {
  CaptureSequenceReassignmentPreview,
  CaptureSequenceMergePreview,
  CaptureSequenceSplitPreview,
  ChronologyExplanation,
  ChronologyImpact,
  ChronologyOverview,
  ClockEpoch,
  ClockEpochClearPreview,
  ClockEpochImpactPreview,
  Contradiction,
  Diagnostic,
  DiagnosticsView,
  TripPhase,
  VisualPlacementPreview,
} from "./chronologyTypes";
import {
  ChronologyCanvas,
  type PhaseBoundaryDraft,
} from "./chronology/ChronologyCanvas";
import { CompareDeck } from "./chronology/CompareDeck";
import { chronologyLanes, formatChronologyTime, type ChronologyLane } from "./chronology/geometry";
import { TripPhaseDialog, type TripPhaseFormValue } from "./components/TripPhaseDialog";

interface Props {
  projectId: string;
  currentAssetId: string | null;
  assets: Asset[];
  overview?: ChronologyOverview | null;
  overviewError?: string;
  reviewZoom?: number;
  reviewScrollLeft?: number;
  focusAssetId?: string | null;
  /** Null in the browser preview, and whenever the loopback origin is down. */
  mediaOrigin?: MediaOrigin | null;
  onBackToReview?(): void;
  onQueueResumed?: (assetId: string) => void;
  onChronologyChanged?: () => void | Promise<void>;
  /** Shared with the Review ribbon, so the two surfaces keep one viewport. */
  onZoomChange?(zoom: number): void;
}

type PendingChange =
  | { kind: "epoch"; label: string; preview: ClockEpochImpactPreview | VisualPlacementPreview; impact: ChronologyImpact }
  | { kind: "reassign"; label: string; preview: CaptureSequenceReassignmentPreview; impact: ChronologyImpact }
  | { kind: "split"; label: string; preview: CaptureSequenceSplitPreview; impact: ChronologyImpact }
  | { kind: "merge"; label: string; preview: CaptureSequenceMergePreview; impact: ChronologyImpact }
  | { kind: "removeEpoch"; label: string; preview: ClockEpochClearPreview; impact: ChronologyImpact };

const emptyDiagnostics: DiagnosticsView = {
  diagnostics: [],
  counts: { errors: 0, warnings: 0, infos: 0 },
};

function hard(contradictions: Contradiction[]) {
  return contradictions.some((found) => found.strength === "hard");
}

function formatOffset(seconds: number): string {
  const sign = seconds < 0 ? "−" : "+";
  const total = Math.abs(seconds);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  if (hours) return `${sign}${hours}h ${minutes}m`;
  return `${sign}${minutes}m`;
}

function sequenceSummary(
  overview: ChronologyOverview,
  lanes: ChronologyLane[],
  sequenceId: string,
) {
  const sequence = overview.sequences.find((item) => item.id === sequenceId);
  if (!sequence) return { label: "No card selected", detail: "Choose a clip on the canvas" };
  // Use the exact solver-first card order drawn on the canvas. The core's
  // declaration list is ID-sorted, so numbering from it can call the same card
  // "Card 1" in the inspector and "Card 2" in the lane.
  const cameraSequences = lanes
    .find((lane) => lane.cameraKey === sequence.cameraKey)
    ?.sequences.map((segment) => segment.sequence)
    ?? overview.sequences.filter((item) => item.cameraKey === sequence.cameraKey);
  const ordinal = cameraSequences.findIndex((item) => item.id === sequence.id) + 1;
  const hint = sequence.volumeHint || sequence.filenamePrefix || "No card hint";
  return {
    label: `Card ${ordinal} of ${cameraSequences.length} · ${sequence.label || sequence.cameraKey}`,
    detail: `${sequence.cameraKey} · ${hint}`,
  };
}

export function CalibrationPanel({
  projectId,
  currentAssetId,
  assets,
  overview: sessionOverview,
  overviewError = "",
  reviewZoom,
  reviewScrollLeft,
  focusAssetId = null,
  mediaOrigin = null,
  onBackToReview,
  onQueueResumed,
  onChronologyChanged,
  onZoomChange,
}: Props) {
  const sessionOwnsOverview = sessionOverview !== undefined;
  const [localOverview, setLocalOverview] = useState<ChronologyOverview | null>(sessionOverview ?? null);
  const overview = sessionOwnsOverview ? sessionOverview : localOverview;
  const [diagnostics, setDiagnostics] = useState<DiagnosticsView>(emptyDiagnostics);
  const [focusedId, setFocusedId] = useState(focusAssetId || currentAssetId || "");
  const [selectedIds, setSelectedIds] = useState<string[]>(focusedId ? [focusedId] : []);
  const [explanation, setExplanation] = useState<ChronologyExplanation | null>(null);
  const [pending, setPending] = useState<PendingChange | null>(null);
  const [phaseDraft, setPhaseDraft] = useState<PhaseBoundaryDraft | null>(null);
  const [phaseBusy, setPhaseBusy] = useState(false);
  const [phaseError, setPhaseError] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [activeTool, setActiveTool] = useState<"why" | "clock" | "sequence">("why");
  const [clockDraft, setClockDraft] = useState({ sequenceId: "", start: "0", end: "", minutes: "0", label: "" });
  const [sequenceTarget, setSequenceTarget] = useState("");
  const [sequenceScope, setSequenceScope] = useState<"single" | "following">("following");
  const [mergeTarget, setMergeTarget] = useState("");
  const [activeProblemIndex, setActiveProblemIndex] = useState(0);
  const [deckCollapsed, setDeckCollapsed] = useState(false);
  const activeProblemIdRef = useRef<string | null>(null);
  const refreshRequestRef = useRef(0);
  const explanationRequestRef = useRef(0);
  const applyingRef = useRef(false);
  const previewRequestRef = useRef(0);
  const impactDockRef = useRef<HTMLElement>(null);
  const previewOriginRef = useRef<HTMLElement | null>(null);

  const assetsById = useMemo(() => new Map(assets.map((asset) => [asset.id, asset])), [assets]);
  // Lane construction is linear in the project's clips. Keep the toolbar
  // count tied to the exact rows the canvas draws without repeating that work
  // on every focus, inspector, or draft-state change.
  const cameraLanes = useMemo(() => overview ? chronologyLanes(overview) : [], [overview]);
  const cameraCount = cameraLanes.length;
  const focusedChronology = overview?.assets.find((asset) => asset.assetId === focusedId) ?? null;
  const currentSequence = focusedChronology?.orderKey.captureSequenceId ?? "";
  const actionableProblems = useMemo(() => [
    ...(overview?.contradictions ?? []).map((item, index) => ({
      id: `contradiction-${index}`,
      severity: item.strength === "hard" ? "error" : "warning",
      message: item.message,
      assetIds: item.assetIds,
      kind: "chronology",
    })),
    ...(overview?.unplacedAssets ? [{
      id: "unplaced",
      severity: "warning",
      message: `${overview.unplacedAssets} clips have no usable time`,
      assetIds: overview.assets.filter((asset) => !asset.tripTime).map((asset) => asset.assetId),
      kind: "chronology",
    }] : []),
  ], [overview]);
  /**
   * Counter restarts are not listed with the problems above. They are not
   * problems: swapping cards through one camera is ordinary practice, and so
   * is a camera that rolls its counter over. The desk cannot tell those apart,
   * so it asks — and a question needs its answers next to it, not a row that
   * only moves the focus and an action buried in the Card tab.
   */
  const cardBoundary = overview?.proposedSplits[0] ?? null;
  const boundarySequence = overview?.sequences.find(
    (sequence) => sequence.id === cardBoundary?.captureSequenceId,
  );
  const focusedProblemIndex = actionableProblems.length
    ? Math.min(activeProblemIndex, actionableProblems.length - 1)
    : -1;

  useEffect(() => {
    const activeProblemId = activeProblemIdRef.current;
    if (!activeProblemId) return;
    const nextIndex = actionableProblems.findIndex((problem) => problem.id === activeProblemId);
    if (nextIndex >= 0) {
      setActiveProblemIndex(nextIndex);
      return;
    }
    activeProblemIdRef.current = null;
    setActiveProblemIndex(-1);
  }, [actionableProblems]);

  useEffect(() => {
    // A target chosen for card A becomes ambiguous or self-referential after
    // the user focuses card B. Require a fresh, visible choice for the new
    // source instead of retaining stale hidden intent.
    setSequenceTarget("");
    setMergeTarget("");
  }, [currentSequence]);

  function visitProblem(delta: number) {
    if (!actionableProblems.length) return;
    const base = focusedProblemIndex >= 0 ? focusedProblemIndex : (delta > 0 ? -1 : 0);
    const nextIndex = (base + delta + actionableProblems.length) % actionableProblems.length;
    const next = actionableProblems[nextIndex];
    setActiveProblemIndex(nextIndex);
    activeProblemIdRef.current = next?.id ?? null;
    const assetId = next?.assetIds[0];
    if (assetId) {
      setFocusedId(assetId);
      setSelectedIds([assetId]);
    }
  }

  const refresh = useCallback(async () => {
    const request = ++refreshRequestRef.current;
    setError("");
    try {
      const [next, found] = await Promise.all([
        sessionOwnsOverview ? Promise.resolve(null) : chronologyApi.overview(projectId),
        chronologyApi.diagnostics(projectId),
      ]);
      if (request !== refreshRequestRef.current) return;
      if (next) setLocalOverview(next);
      setDiagnostics(found);
    } catch (cause) {
      if (request === refreshRequestRef.current) setError(String(cause));
    }
  }, [projectId, sessionOwnsOverview]);

  useEffect(() => {
    setPending(null);
    setPhaseDraft(null);
    setActiveProblemIndex(0);
    activeProblemIdRef.current = null;
    setFocusedId(focusAssetId || currentAssetId || "");
    setSelectedIds(focusAssetId || currentAssetId ? [focusAssetId || currentAssetId || ""] : []);
    void refresh();
    return () => {
      refreshRequestRef.current += 1;
      explanationRequestRef.current += 1;
      previewRequestRef.current += 1;
    };
  }, [currentAssetId, focusAssetId, projectId, refresh]);

  useEffect(() => {
    const request = ++explanationRequestRef.current;
    if (!focusedId) {
      setExplanation(null);
      return;
    }
    void chronologyApi.explain(focusedId).then((next) => {
      if (request === explanationRequestRef.current) setExplanation(next);
    }).catch(() => {
      if (request === explanationRequestRef.current) setExplanation(null);
    });
  }, [focusedId, overview]);

  useEffect(() => {
    if (!pending) return;
    window.requestAnimationFrame(() => {
      impactDockRef.current?.querySelector<HTMLButtonElement>(".chronology-impact-actions .primary")?.focus();
    });
    function cancelPreview(event: KeyboardEvent) {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      cancelPending();
    }
    document.addEventListener("keydown", cancelPreview);
    return () => document.removeEventListener("keydown", cancelPreview);
  }, [pending]);

  function rememberPreviewOrigin() {
    previewOriginRef.current = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
  }

  function cancelPending() {
    previewRequestRef.current += 1;
    setPending(null);
    window.requestAnimationFrame(() => previewOriginRef.current?.focus());
  }

  async function refreshAfterMutation() {
    await onChronologyChanged?.();
    await refresh();
  }

  async function applyPending() {
    if (!pending || applyingRef.current || pending.impact.blocked) return;
    applyingRef.current = true;
    setBusy(true);
    setError("");
    try {
      const position = currentAssetId
        ? await chronologyApi.captureQueuePosition(projectId, currentAssetId)
        : null;
      if (pending.kind === "epoch") {
        await chronologyApi.setClockEpoch(projectId, pending.preview);
      } else if (pending.kind === "reassign") {
        await chronologyApi.reassignCaptureSequenceAssets(projectId, pending.preview);
      } else if (pending.kind === "split") {
        await chronologyApi.applyCaptureSequenceSplit(projectId, pending.preview);
      } else if (pending.kind === "merge") {
        await chronologyApi.applyCaptureSequenceMerge(projectId, pending.preview);
      } else {
        await chronologyApi.clearClockEpoch(projectId, pending.preview);
      }
      const changed = pending.impact.changes.length;
      setPending(null);
      await refreshAfterMutation();
      if (position) {
        const resumed = await chronologyApi.restoreQueuePosition(projectId, position);
        if (resumed) onQueueResumed?.(resumed);
      }
      setNotice(`${pending.label} · ${changed} ${changed === 1 ? "clip" : "clips"} affected`);
    } catch (cause) {
      setError(String(cause));
    } finally {
      applyingRef.current = false;
      setBusy(false);
    }
  }

  async function previewClock() {
    if (!clockDraft.sequenceId) return;
    rememberPreviewOrigin();
    setBusy(true);
    setError("");
    const request = ++previewRequestRef.current;
    const epoch: ClockEpoch = {
      id: "",
      captureSequenceId: clockDraft.sequenceId,
      startCounter: Number(clockDraft.start) || 0,
      endCounter: clockDraft.end.trim() ? Number(clockDraft.end) : null,
      offsetSeconds: Math.round((Number(clockDraft.minutes) || 0) * 60),
      confidence: 1,
      evidence: ["user_set"],
      label: clockDraft.label.trim() || "Camera clock correction",
    };
    try {
      const preview = await chronologyApi.previewClockEpochImpact(projectId, epoch);
      if (request === previewRequestRef.current) {
        setPending({ kind: "epoch", label: preview.epoch.label, preview, impact: preview.impact });
      }
    } catch (cause) {
      if (request === previewRequestRef.current) setError(String(cause));
    } finally {
      if (request === previewRequestRef.current) setBusy(false);
    }
  }

  async function previewVisualPlacement(targetAssetId: string, side: "before" | "after") {
    if (!selectedIds.length) return;
    rememberPreviewOrigin();
    setBusy(true);
    setError("");
    const request = ++previewRequestRef.current;
    try {
      const preview: VisualPlacementPreview = await chronologyApi.previewVisualPlacement(
        projectId,
        selectedIds,
        targetAssetId,
        side,
        "Placed visually in Chronology",
      );
      if (request === previewRequestRef.current) {
        setPending({ kind: "epoch", label: "Move selected run", preview, impact: preview.impact });
      }
    } catch (cause) {
      if (request === previewRequestRef.current) {
        setError(`${String(cause)}. Try pinning one clip or choosing a run from one card.`);
      }
    } finally {
      if (request === previewRequestRef.current) setBusy(false);
    }
  }

  async function previewReassignment() {
    if (!focusedId || !sequenceTarget) return;
    rememberPreviewOrigin();
    setBusy(true);
    setError("");
    const request = ++previewRequestRef.current;
    try {
      const preview = sequenceScope === "single"
        ? await chronologyApi.previewCaptureSequenceAssets(
            projectId,
            currentSequence,
            sequenceTarget,
            [focusedId],
          )
        : await chronologyApi.previewCaptureSequenceReassignment(
            projectId,
            focusedId,
            sequenceTarget,
          );
      if (request === previewRequestRef.current) {
        setPending({
          kind: "reassign",
          label: sequenceScope === "single" ? "Move this clip to another card" : "Move clip run to another card",
          preview,
          impact: preview.impact,
        });
      }
    } catch (cause) {
      if (request === previewRequestRef.current) setError(String(cause));
    } finally {
      if (request === previewRequestRef.current) setBusy(false);
    }
  }

  async function previewSplit(proposal: ChronologyOverview["proposedSplits"][number]) {
    rememberPreviewOrigin();
    setBusy(true);
    setError("");
    const request = ++previewRequestRef.current;
    try {
      const source = overview?.sequences.find((sequence) => sequence.id === proposal.captureSequenceId);
      const label = `${source?.label || source?.cameraKey || "Card"} · new card`;
      const preview = await chronologyApi.previewCaptureSequenceSplit(
        projectId,
        proposal.captureSequenceId,
        proposal.atCounter,
        label,
        proposal.affectedAssetIds,
      );
      if (request === previewRequestRef.current) {
        setPending({ kind: "split", label: `Create ${preview.createdSequence.label}`, preview, impact: preview.impact });
      }
    } catch (cause) {
      if (request === previewRequestRef.current) setError(String(cause));
    } finally {
      if (request === previewRequestRef.current) setBusy(false);
    }
  }

  async function previewMerge() {
    if (!currentSequence || !mergeTarget) return;
    rememberPreviewOrigin();
    setBusy(true);
    setError("");
    const request = ++previewRequestRef.current;
    try {
      const preview = await chronologyApi.previewCaptureSequenceMerge(projectId, currentSequence, mergeTarget);
      const target = overview?.sequences.find((sequence) => sequence.id === mergeTarget);
      if (request === previewRequestRef.current) {
        setPending({ kind: "merge", label: `Merge into ${target?.label || target?.cameraKey || "card"}`, preview, impact: preview.impact });
      }
    } catch (cause) {
      if (request === previewRequestRef.current) setError(String(cause));
    } finally {
      if (request === previewRequestRef.current) setBusy(false);
    }
  }

  async function previewRemoveEpoch(epoch: ClockEpoch) {
    const label = `Remove ${epoch.label || "correction"}`;
    rememberPreviewOrigin();
    setBusy(true);
    const request = ++previewRequestRef.current;
    try {
      const preview = await chronologyApi.previewClearClockEpochImpact(projectId, epoch.id);
      if (request === previewRequestRef.current) {
        setPending({ kind: "removeEpoch", label, preview, impact: preview.impact });
      }
    } catch (cause) {
      if (request === previewRequestRef.current) setError(String(cause));
    } finally {
      if (request === previewRequestRef.current) setBusy(false);
    }
  }

  async function savePhase(
    value: TripPhaseFormValue,
    range?: { startPosition: number; endPosition: number },
  ) {
    if (!phaseDraft || !overview) return;
    const startIndex = range?.startPosition ?? phaseDraft.startIndex;
    const endIndex = range?.endPosition ?? phaseDraft.endIndex;
    const start = overview.assets[startIndex];
    const end = overview.assets[endIndex];
    if (!start || !end || startIndex > endIndex) return;
    setPhaseBusy(true);
    setPhaseError("");
    try {
      await chronologyApi.setTripPhase(projectId, {
        ...phaseDraft.phase,
        label: value.label,
        kind: value.kind,
        startAnchorAssetId: start.assetId,
        endAnchorAssetId: end.assetId,
        startTripTime: start.tripTime ?? phaseDraft.phase.startTripTime,
        endTripTime: end.tripTime ?? phaseDraft.phase.endTripTime,
        timezone: value.timezone,
        originTimezone: value.originTimezone,
        destinationTimezone: value.destinationTimezone,
        displayMode: value.kind === "location" ? "local" : value.displayMode,
      });
      setPhaseDraft(null);
      setNotice(`Adjusted ${value.label} to ${endIndex - startIndex + 1} clips`);
      await refreshAfterMutation();
    } catch (cause) {
      setPhaseError(String(cause));
    } finally {
      setPhaseBusy(false);
    }
  }

  /**
   * "Yes, new card." Goes through the same preview-then-confirm every other
   * chronology write uses; the split really does move clips to a new sequence.
   */
  async function acceptCardBoundary() {
    if (!cardBoundary) return;
    await previewSplit(cardBoundary);
  }

  /**
   * "No, one card." Records the answer and nothing else.
   *
   * No impact preview, deliberately: the clips were already in one sequence and
   * stay there, so there is no placement change to show and a confirm dialog
   * would be ceremony over nothing. It stays reversible from the Card tab.
   */
  async function declineCardBoundary() {
    if (!cardBoundary) return;
    setBusy(true);
    setError("");
    try {
      await chronologyApi.declineCaptureSequenceSplit(
        projectId,
        cardBoundary.captureSequenceId,
        cardBoundary.atCounter,
        "Same card — the counter restarted on its own",
      );
      await refreshAfterMutation();
      setNotice("Kept as one card. Reopen it from the Card tab if that was wrong.");
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }

  async function restoreCardBoundary(captureSequenceId: string, atCounter: number) {
    setBusy(true);
    setError("");
    try {
      await chronologyApi.restoreCaptureSequenceSplit(projectId, captureSequenceId, atCounter);
      await refreshAfterMutation();
      setNotice(`Asking about the restart at ${atCounter} again`);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }

  /** Focuses the first clip of the run after the restart, so the deck pairs it
   *  with the last clip before it — the comparison that answers the question. */
  function reviewCardBoundary() {
    const first = cardBoundary?.affectedAssetIds[0];
    if (!first) return;
    setFocusedId(first);
    setSelectedIds([first]);
  }

  async function acknowledge(diagnostic: Diagnostic) {
    await chronologyApi.acknowledgeDiagnostic(projectId, diagnostic.id);
    await refresh();
  }

  async function locate(diagnostic: Diagnostic) {
    const context = diagnostic.context as { volumeHint?: string } | null;
    const folder = await api.chooseRelinkFolder(context?.volumeHint || "these clips");
    if (!folder) return;
    setBusy(true);
    try {
      const report = await api.relinkFolder(projectId, folder);
      setNotice(`Reconnected ${report.repaired} clips`);
      await refreshAfterMutation();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }

  if (!overview) {
    return (
      <section className="chronology-workspace-loading" aria-label="Chronology calibration" aria-busy="true">
        <span className="chronology-loader" aria-hidden="true" />
        <h2>{overviewError ? "Chronology could not be loaded" : "Building the chronology desk…"}</h2>
        {overviewError && <p role="alert">{overviewError}</p>}
        {(error || overviewError) && <button className="button" onClick={() => void onChronologyChanged?.()}>Try again</button>}
      </section>
    );
  }

  const focusedAsset = assetsById.get(focusedId) ?? assets[0];
  const zoom = reviewZoom ?? RIBBON_ZOOM_DEFAULT;
  const focusedIndex = overview.assets.findIndex((asset) => asset.assetId === focusedId);
  const currentCard = sequenceSummary(overview, cameraLanes, currentSequence);
  const focusedPhase = overview.phases.find(
    (phase) => phase.id === focusedChronology?.displayTime?.phaseId,
  );

  return (
    <section className="chronology-workspace" aria-label="Chronology calibration">
      <header className="chronology-toolbar">
        <button type="button" className="chronology-back" onClick={onBackToReview}>
          <span aria-hidden="true">←</span>
          Back to {currentAssetId ? assetsById.get(currentAssetId)?.filename ?? "review clip" : "Review"}
        </button>
        <div className="chronology-title">
          <span>Chronology desk</span>
          <strong>{actionableProblems.length
            ? `${actionableProblems.length} ${actionableProblems.length === 1 ? "problem" : "problems"}`
            : cardBoundary
              ? "1 card boundary to confirm"
              : "Order looks trustworthy"}</strong>
        </div>
        <div className="chronology-trust-summary" aria-label="Chronology summary">
          {actionableProblems.length > 0 && (
            <div className="chronology-problem-nav" aria-label="Problem navigation">
              <button type="button" disabled={Boolean(pending)} aria-label="Previous chronology problem" onClick={() => visitProblem(-1)}>←</button>
              <span>{focusedProblemIndex >= 0 ? focusedProblemIndex + 1 : 1}/{actionableProblems.length}</span>
              <button type="button" disabled={Boolean(pending)} aria-label="Next chronology problem" onClick={() => visitProblem(1)}>→</button>
            </div>
          )}
          <span>{overview.assets.length} clips</span>
          <span>{cameraCount} {cameraCount === 1 ? "camera" : "cameras"}</span>
          <span>{overview.sequences.length} cards</span>
          <span>{overview.phases.length} phases</span>
          {overview.unplacedAssets > 0 && <strong>{overview.unplacedAssets} unplaced</strong>}
        </div>
      </header>

      {(error || notice) && (
        <div className={`chronology-toast ${error ? "is-error" : ""}`} role={error ? "alert" : "status"}>
          <span>{error || notice}</span>
          <button type="button" aria-label="Dismiss message" onClick={() => { setError(""); setNotice(""); }}>×</button>
        </div>
      )}

      <aside className="chronology-problem-rail" aria-label="Chronology problems">
        <div className="chronology-rail-heading">
          <span className="eyebrow">Worklist</span>
          <h2>{actionableProblems.length ? "Needs a look" : "All in order"}</h2>
        </div>
        {actionableProblems.length === 0 ? (
          <div className="chronology-clean-state">
            <span aria-hidden="true">✓</span>
            <strong>No known ordering conflicts</strong>
            <p>You can still inspect any clip or make an explicit correction.</p>
          </div>
        ) : (
          <ol className="chronology-problem-list">
            {actionableProblems.map((problem, index) => (
              <li key={problem.id} data-severity={problem.severity}>
                <button
                  type="button"
                  disabled={Boolean(pending)}
                  aria-label={`Problem ${index + 1}: ${problem.message}, ${problem.assetIds.length ? `${problem.assetIds.length} clips` : "project"}`}
                  title={problem.message}
                  onClick={() => {
                    setActiveProblemIndex(index);
                    activeProblemIdRef.current = problem.id;
                    const assetId = problem.assetIds[0];
                    if (assetId) {
                      setFocusedId(assetId);
                      setSelectedIds([assetId]);
                    }
                  }}
                >
                  <span>{String(index + 1).padStart(2, "0")}</span>
                  <strong>{problem.message}</strong>
                  <em>{problem.assetIds.length ? `${problem.assetIds.length} clips` : "Project"}</em>
                </button>
              </li>
            ))}
          </ol>
        )}
        {cardBoundary && (
          <div className="chronology-card-boundary" role="group" aria-label="Card boundary question">
            <span className="eyebrow">Card boundary</span>
            <h3>Did you change cards here?</h3>
            <p>
              The filename counter restarts at <strong>{cardBoundary.atCounter}</strong> partway
              through {boundarySequence?.label || boundarySequence?.cameraKey || "this card"}.
              That is what a second card looks like — and also what a camera that rolls its own
              counter over looks like.
            </p>
            <button
              type="button"
              className="chronology-boundary-review"
              disabled={Boolean(pending) || busy}
              onClick={reviewCardBoundary}
            >
              Review this boundary on the deck
              <em>{cardBoundary.affectedAssetIds.length} clips follow it</em>
            </button>
            <div className="chronology-boundary-actions">
              <button
                type="button"
                className="button primary"
                disabled={Boolean(pending) || busy}
                onClick={() => void acceptCardBoundary()}
              >
                Yes — new card
              </button>
              <button
                type="button"
                className="button"
                disabled={Boolean(pending) || busy}
                onClick={() => void declineCardBoundary()}
              >
                No — one card
              </button>
            </div>
          </div>
        )}

        {diagnostics.diagnostics.length > 0 && (
          <details className="chronology-media-problems">
            <summary>Media and catalog notes · {diagnostics.diagnostics.length}</summary>
            {diagnostics.diagnostics.map((diagnostic) => (
              <div key={diagnostic.id}>
                <p>{diagnostic.message}</p>
                {diagnostic.kind === "media.volume_offline" && <button onClick={() => void locate(diagnostic)}>Locate originals…</button>}
                <button onClick={() => void acknowledge(diagnostic)}>Acknowledge</button>
              </div>
            ))}
          </details>
        )}
      </aside>

      <main className="chronology-canvas-panel">
        <div className="chronology-canvas-heading">
          <div>
            <span className="eyebrow">Solver order · one lane per camera</span>
            <h2>{selectedIds.length > 1 ? `${selectedIds.length} clips selected` : focusedAsset?.filename ?? "Select a clip"}</h2>
          </div>
          <p>Shift-click clips on one card, then choose a cyan gap to place the run.</p>
          {onZoomChange && (
            <div className="chronology-zoom" aria-label="Tile size">
              <button
                type="button"
                aria-label="Smaller tiles"
                disabled={zoom <= RIBBON_ZOOM_MIN}
                onClick={() => onZoomChange(Math.max(RIBBON_ZOOM_MIN, zoom - RIBBON_ZOOM_STEP))}
              >−</button>
              <input
                type="range"
                min={RIBBON_ZOOM_MIN}
                max={RIBBON_ZOOM_MAX}
                step={RIBBON_ZOOM_STEP}
                value={zoom}
                aria-label="Tile size"
                onChange={(event) => onZoomChange(Number(event.target.value))}
              />
              <button
                type="button"
                aria-label="Larger tiles"
                disabled={zoom >= RIBBON_ZOOM_MAX}
                onClick={() => onZoomChange(Math.min(RIBBON_ZOOM_MAX, zoom + RIBBON_ZOOM_STEP))}
              >+</button>
            </div>
          )}
        </div>
        <CompareDeck
          overview={overview}
          assetsById={assetsById}
          focusedAssetId={focusedId || null}
          mediaOrigin={mediaOrigin}
          collapsed={deckCollapsed}
          disabled={busy || Boolean(pending)}
          onFocusAsset={(assetId) => { setFocusedId(assetId); setSelectedIds([assetId]); }}
          onToggleCollapsed={() => setDeckCollapsed((current) => !current)}
        />
        <ChronologyCanvas
          overview={overview}
          assetsById={assetsById}
          focusedAssetId={focusedId}
          selectedIds={selectedIds}
          impact={pending?.impact ?? null}
          columnWidth={reviewZoom}
          initialScrollLeft={reviewScrollLeft}
          disabled={busy || Boolean(pending)}
          onFocusAsset={setFocusedId}
          onSelectRange={setSelectedIds}
          onPlaceSelection={(target, side) => void previewVisualPlacement(target, side)}
          onAdjustPhase={setPhaseDraft}
        />
      </main>

      <aside className="chronology-inspector" aria-label="Chronology inspector">
        <div className="chronology-inspector-tabs" aria-label="Correction tools">
          {(["why", "clock", "sequence"] as const).map((tool) => (
            <button key={tool} disabled={Boolean(pending)} aria-pressed={activeTool === tool} onClick={() => setActiveTool(tool)}>
              {tool === "why" ? "Why here" : tool === "clock" ? "Clock" : "Card"}
            </button>
          ))}
        </div>

        {activeTool === "why" && (
          <div className="chronology-inspector-body">
            <span className="eyebrow">Clip {focusedIndex >= 0 ? focusedIndex + 1 : "—"}</span>
            <h2>{focusedAsset?.filename ?? "Choose a clip"}</h2>
            {focusedChronology && (
              <>
                <dl className="chronology-time-pair">
                  <div><dt>Camera said</dt><dd>{formatChronologyTime(focusedChronology.rawTime)}</dd></div>
                  <div><dt>Placed at</dt><dd>{formatChronologyTime(focusedChronology.displayTime?.primary ?? focusedChronology.tripTime, focusedChronology.displayTime?.timezone)}</dd></div>
                  {focusedChronology.displayTime?.secondary && (
                    <div><dt>Destination</dt><dd>{formatChronologyTime(focusedChronology.displayTime.secondary, focusedPhase?.destinationTimezone)}</dd></div>
                  )}
                </dl>
                <div className="chronology-confidence">
                  <span><i style={{ width: `${Math.round(focusedChronology.confidence * 100)}%` }} /></span>
                  <strong>{Math.round(focusedChronology.confidence * 100)}% confidence</strong>
                  <em>{focusedChronology.uncertaintySeconds === 0 ? "Exact" : `± ${focusedChronology.uncertaintySeconds}s`}</em>
                </div>
              </>
            )}
            {explanation && (
              <div className="chronology-evidence-list">
                <h3>Evidence and corrections</h3>
                {explanation.appliedCorrections.length === 0 ? <p>No explicit corrections.</p> : (
                  <ol>{explanation.appliedCorrections.map((correction, index) => (
                    <li key={`${correction.type}-${index}`}>
                      <span>{index + 1}</span>
                      <p>
                        {correction.type === "captureSequence" && `Grouped with ${correction.label}`}
                        {correction.type === "clockEpoch" && `Clock shifted ${formatOffset(correction.offsetSeconds)}${correction.label ? ` · ${correction.label}` : ""}`}
                        {correction.type === "clipException" && `Pinned at ${formatChronologyTime(correction.tripTime)} · ${correction.reason}`}
                        {correction.type === "tripPhase" && `Displayed in ${correction.timezone} · ${correction.label}`}
                        {correction.type === "eventOrder" && `${correction.strength === "hard" ? "Must" : "Should"} stay relative to another clip`}
                      </p>
                    </li>
                  ))}</ol>
                )}
              </div>
            )}
          </div>
        )}

        {activeTool === "clock" && (
          <form className="chronology-tool-form" onSubmit={(event) => { event.preventDefault(); void previewClock(); }}>
            <h2>Correct a camera clock</h2>
            <p>Use this when a camera stayed on home time or changed mid-card.</p>
            <label>Card / capture sequence<select required value={clockDraft.sequenceId} onChange={(event) => setClockDraft({ ...clockDraft, sequenceId: event.target.value })}><option value="">Choose a card…</option>{overview.sequences.map((sequence) => <option key={sequence.id} value={sequence.id}>{sequenceSummary(overview, cameraLanes, sequence.id).label}</option>)}</select></label>
            <div className="chronology-form-pair">
              <label>From clip number<input type="number" min={0} value={clockDraft.start} onChange={(event) => setClockDraft({ ...clockDraft, start: event.target.value })} /></label>
              <label>Through (optional)<input type="number" min={0} value={clockDraft.end} onChange={(event) => setClockDraft({ ...clockDraft, end: event.target.value })} /></label>
            </div>
            <label>Shift by minutes<input type="number" value={clockDraft.minutes} onChange={(event) => setClockDraft({ ...clockDraft, minutes: event.target.value })} /></label>
            <label>Reason<input value={clockDraft.label} onChange={(event) => setClockDraft({ ...clockDraft, label: event.target.value })} placeholder="Camera never left home time" /></label>
            <button className="button primary" disabled={busy || Boolean(pending) || !clockDraft.sequenceId}>Preview correction</button>
            {overview.epochs.length > 0 && <div className="chronology-existing-facts"><h3>Existing corrections</h3>{overview.epochs.map((epoch) => <button type="button" disabled={Boolean(pending)} key={epoch.id} onClick={() => void previewRemoveEpoch(epoch)}><span>{epoch.label || "Clock correction"}</span><em>{formatOffset(epoch.offsetSeconds)} · remove…</em></button>)}</div>}
          </form>
        )}

        {activeTool === "sequence" && (
          <div className="chronology-tool-form">
            <h2>Fix card grouping</h2>
            <p>Move one clip or the following filename run to the card it belongs to.</p>
            <div
              className="chronology-current-card"
              role="group"
              aria-label={`Source card: ${currentCard.label}, ${currentCard.detail}`}
            >
              <span>Source card</span>
              <strong>{currentCard.label}</strong>
              <em>{currentCard.detail}</em>
            </div>
            <fieldset className="chronology-scope-picker">
              <legend>Move scope</legend>
              <label><input type="radio" name="sequence-scope" checked={sequenceScope === "single"} onChange={() => setSequenceScope("single")} /> This clip only</label>
              <label><input type="radio" name="sequence-scope" checked={sequenceScope === "following"} onChange={() => setSequenceScope("following")} /> This clip and following</label>
            </fieldset>
            <label>Destination card<select value={sequenceTarget} onChange={(event) => setSequenceTarget(event.target.value)}><option value="">Choose a card…</option>{overview.sequences.filter((sequence) => sequence.id !== currentSequence).map((sequence) => <option key={sequence.id} value={sequence.id}>{sequenceSummary(overview, cameraLanes, sequence.id).label}</option>)}</select></label>
            <button type="button" className="button primary" disabled={busy || Boolean(pending) || !focusedId || !sequenceTarget} onClick={() => void previewReassignment()}>Preview card move</button>
            <label>Merge this whole card into<select value={mergeTarget} onChange={(event) => setMergeTarget(event.target.value)}><option value="">Choose a card…</option>{overview.sequences.filter((sequence) => sequence.id !== currentSequence).map((sequence) => <option key={sequence.id} value={sequence.id}>{sequenceSummary(overview, cameraLanes, sequence.id).label}</option>)}</select></label>
            <button type="button" className="button" disabled={busy || Boolean(pending) || !currentSequence || !mergeTarget} onClick={() => void previewMerge()}>Preview whole-card merge</button>
            {overview.declinedSplits.length > 0 && (
              <div className="chronology-existing-facts">
                <h3>Counter restarts you kept as one card</h3>
                {overview.declinedSplits.map((declined) => {
                  const sequence = overview.sequences.find((item) => item.id === declined.captureSequenceId);
                  return (
                    <button
                      type="button"
                      disabled={Boolean(pending) || busy}
                      key={`${declined.captureSequenceId}-${declined.atCounter}`}
                      aria-label={`Ask about the restart at ${declined.atCounter} again`}
                      onClick={() => void restoreCardBoundary(declined.captureSequenceId, declined.atCounter)}
                    >
                      <span>{sequence?.label || sequence?.cameraKey || "Card"} · restart at {declined.atCounter}</span>
                      <em>Ask again…</em>
                    </button>
                  );
                })}
              </div>
            )}
          </div>
        )}
      </aside>

      {pending && (
        <section ref={impactDockRef} className="chronology-impact-dock" aria-label="Chronology impact preview" role="dialog" aria-modal="false">
          <div className="chronology-impact-summary">
            <span className="eyebrow">Nothing has changed yet</span>
            <h2>{pending.label}</h2>
            <p><strong>{pending.impact.changes.length}</strong> {pending.impact.changes.length === 1 ? "clip moves" : "clips move"}</p>
          </div>
          <div className="chronology-impact-rows">
            {pending.impact.changes.length === 0 ? <p>No clip placement changes.</p> : pending.impact.changes.slice(0, 6).map((change) => (
              <button type="button" key={change.assetId} onClick={() => setFocusedId(change.assetId)}>
                <strong>{assetsById.get(change.assetId)?.filename ?? change.assetId}</strong>
                <span>#{change.beforePosition + 1} → #{change.afterPosition + 1}</span>
                <em>{formatChronologyTime(change.before)} → {formatChronologyTime(change.after)}</em>
              </button>
            ))}
            {pending.impact.changes.length > 6 && <p>+ {pending.impact.changes.length - 6} more affected clips</p>}
          </div>
          <div className="chronology-impact-verdict">
            {pending.impact.contradictions.length > 0 ? pending.impact.contradictions.map((item, index) => <p key={index} data-strength={item.strength}>{item.message}</p>) : <p className="is-clean">No contradictions introduced.</p>}
          </div>
          <div className="chronology-impact-actions">
            <button type="button" className="button" disabled={busy} onClick={cancelPending}>Cancel</button>
            <button type="button" className="button primary" disabled={busy || pending.impact.blocked || hard(pending.impact.contradictions)} onClick={() => void applyPending()}>{busy ? "Applying…" : "Apply change"}</button>
          </div>
        </section>
      )}

      {phaseDraft && overview.assets[phaseDraft.startIndex] && overview.assets[phaseDraft.endIndex] && assetsById.get(overview.assets[phaseDraft.startIndex].assetId) && assetsById.get(overview.assets[phaseDraft.endIndex].assetId) && (
        <TripPhaseDialog
          phase={phaseDraft.phase}
          start={assetsById.get(overview.assets[phaseDraft.startIndex].assetId)!}
          end={assetsById.get(overview.assets[phaseDraft.endIndex].assetId)!}
          startPosition={phaseDraft.startIndex}
          endPosition={phaseDraft.endIndex}
          assets={overview.assets.map((item) => assetsById.get(item.assetId)).filter((item): item is Asset => Boolean(item))}
          busy={phaseBusy}
          error={phaseError}
          onCancel={() => setPhaseDraft(null)}
          onSave={(value, range) => void savePhase(value, range)}
        />
      )}
    </section>
  );
}
