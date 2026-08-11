/**
 * One owner for everything the open project reads.
 *
 * Before this hook, `App` loaded the queue, the chronology overview, the
 * analysis index, the readiness summary, and the selected clip's evidence from
 * five separate effects keyed on `projectId` / `selectedId`. Two consequences
 * followed from there being no other way to start a read:
 *
 *   - a read that failed could not be tried again, because "read that again"
 *     was not expressible (#18);
 *   - a read that landed late could overwrite a newer project's state, because
 *     nothing associated a response with the session that asked for it (#28).
 *
 * Both are answered here. Every read carries the session generation that
 * started it, and a response from an older generation is dropped — it cannot
 * write assets, chronology, evidence, loading, or the error. Invalidation is
 * explicit: `reload()` after an analysis run, `reloadAssets()` after a write
 * that changes the queue, `reloadEvidence()` for the clip on screen.
 *
 * There is deliberately no `reloadAsset(id)`. No backend command reads a single
 * asset, and inventing one is a Rust change; `reloadAssets()` is the honest
 * granularity the IPC surface actually offers.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api";
import { chronologyApi } from "../chronologyApi";
import type { ChronologyOverview } from "../chronologyTypes";
import type {
  AnalysisReadiness,
  Asset,
  AssetAnalysisIndexEntry,
  AssetAnalysisView,
  ClipTranscript,
} from "../types";

export interface ProjectSessionOptions {
  /** The open project. Changing it starts a new session; older reads stop counting. */
  projectId: string;
  /** The clip whose evidence the session keeps loaded. */
  selectedId: string;
  /** Where a failed project read is reported to the user. */
  onError(message: string): void;
  /** Called when the queue read lands, with what it returned. */
  onAssetsRead?(assets: Asset[]): void;
}

export interface ProjectSession {
  assets: Asset[];
  chronology: ChronologyOverview | null;
  analysisIndex: Record<string, AssetAnalysisIndexEntry>;
  readiness: AnalysisReadiness | null;
  transcript: ClipTranscript | null;
  observation: AssetAnalysisView | null;
  /** True while the project's reads are in flight. */
  loading: boolean;
  /** Non-empty when the queue read failed; what the retry affordance hangs on. */
  loadError: string;
  /** Re-read everything this session owns, including the selected clip's evidence. */
  reload(): Promise<void>;
  /** Re-read the queue alone — what a write that changes membership or order needs. */
  reloadAssets(): Promise<void>;
  /** Re-read the chronology overview alone — what a trip-phase write changes. */
  reloadChronology(): Promise<void>;
  /** Re-read the analysis readiness summary alone — what the run dialog is gated on. */
  reloadReadiness(): Promise<void>;
  /** Re-read the selected clip's transcript and observation. */
  reloadEvidence(): Promise<void>;
  /** Replace one asset in place, keeping the queue's order. Optimistic writes and progress events. */
  applyAsset(asset: Asset): void;
  /** Rewrite the queue from the current one, for the batch listeners that touch many rows. */
  patchAssets(update: (current: Asset[]) => Asset[]): void;
}

type ReadKind = "assets" | "chronology" | "analysisIndex" | "readiness" | "evidence";

interface ReadToken {
  kind: ReadKind;
  session: number;
  sequence: number;
}

function indexAnalyses(entries: AssetAnalysisIndexEntry[]): Record<string, AssetAnalysisIndexEntry> {
  return Object.fromEntries(entries.map((entry) => [entry.assetId, entry]));
}

function sortAssets(assets: Asset[]): Asset[] {
  return [...assets].sort((a, b) =>
    (a.bestTime ?? `z${a.filename}`).localeCompare(b.bestTime ?? `z${b.filename}`),
  );
}

export function useProjectSession({
  projectId,
  selectedId,
  onError,
  onAssetsRead,
}: ProjectSessionOptions): ProjectSession {
  const [assets, setAssets] = useState<Asset[]>([]);
  const [chronology, setChronology] = useState<ChronologyOverview | null>(null);
  const [analysisIndex, setAnalysisIndex] = useState<Record<string, AssetAnalysisIndexEntry>>({});
  const [readiness, setReadiness] = useState<AnalysisReadiness | null>(null);
  const [transcript, setTranscript] = useState<ClipTranscript | null>(null);
  const [observation, setObservation] = useState<AssetAnalysisView | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState("");

  /**
   * The session generation. It moves when the open project changes and when the
   * session unmounts — and at no other time, because it is the thing a response
   * is checked against to answer "is this still the project the user is in?".
   */
  const sessionRef = useRef(0);
  /**
   * The latest read of each kind. Reloads are independent of one another: a
   * readiness reload issued alongside a queue reload must not cancel it, which
   * is why this is per kind rather than one shared counter.
   */
  const sequenceRef = useRef<Record<ReadKind, number>>({
    assets: 0,
    chronology: 0,
    analysisIndex: 0,
    readiness: 0,
    evidence: 0,
  });

  const projectIdRef = useRef(projectId);
  const selectedIdRef = useRef(selectedId);
  projectIdRef.current = projectId;
  selectedIdRef.current = selectedId;

  // Held in refs so a caller passing an inline closure does not restart every
  // read on each render.
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;
  const onAssetsReadRef = useRef(onAssetsRead);
  onAssetsReadRef.current = onAssetsRead;

  /** Claim the next read of this kind, stamped with the session that asked for it. */
  const begin = useCallback((kind: ReadKind): ReadToken => {
    sequenceRef.current[kind] += 1;
    return { kind, session: sessionRef.current, sequence: sequenceRef.current[kind] };
  }, []);

  /**
   * Whether a response may still write state. Two ways it may not: the user has
   * opened another project, or a newer read of the same kind has been issued.
   */
  const isCurrent = useCallback(
    (token: ReadToken) =>
      token.session === sessionRef.current && token.sequence === sequenceRef.current[token.kind],
    [],
  );

  const loadEvidence = useCallback(async (token: ReadToken, assetId: string) => {
    if (!assetId) return;
    try {
      const [loadedTranscript, loadedObservation] = await Promise.all([
        api.assetTranscript(assetId),
        api.assetAnalysis(assetId),
      ]);
      if (!isCurrent(token)) return;
      setTranscript(loadedTranscript);
      setObservation(loadedObservation);
    } catch {
      if (!isCurrent(token)) return;
      setTranscript(null);
      setObservation(null);
    }
  }, [isCurrent]);

  const loadAssets = useCallback(async (token: ReadToken, activeProjectId: string) => {
    try {
      const result = await api.listAssets(activeProjectId);
      if (!isCurrent(token)) return;
      // The backend's order is the queue's order; only a per-asset replacement
      // re-sorts, and it does so the way `App` always has.
      setAssets(result);
      setLoadError("");
      onAssetsReadRef.current?.(result);
    } catch (reason) {
      if (!isCurrent(token)) return;
      const message = String(reason);
      setAssets([]);
      setLoadError(message);
      onErrorRef.current(message);
    }
  }, [isCurrent]);

  /**
   * The summaries beside the queue. Each fails to its empty value on its own: a
   * chronology the solver cannot produce should not blank the queue, and never
   * did.
   */
  const settleRead = useCallback(
    async <T,>(token: ReadToken, read: () => Promise<T>, apply: (value: T) => void, empty: T) => {
      try {
        const value = await read();
        if (!isCurrent(token)) return;
        apply(value);
      } catch {
        if (!isCurrent(token)) return;
        apply(empty);
      }
    },
    [isCurrent],
  );

  const loadAll = useCallback(
    async (activeProjectId: string) => {
      const assetsToken = begin("assets");
      const chronologyToken = begin("chronology");
      const analysisToken = begin("analysisIndex");
      const readinessToken = begin("readiness");
      const evidenceToken = begin("evidence");
      if (!activeProjectId) {
        if (assetsToken.session !== sessionRef.current) return;
        setAssets([]);
        setChronology(null);
        setAnalysisIndex({});
        setReadiness(null);
        setLoadError("");
        setLoading(false);
        return;
      }
      setLoading(true);
      await Promise.all([
        loadAssets(assetsToken, activeProjectId),
        settleRead(chronologyToken, () => chronologyApi.overview(activeProjectId), setChronology, null),
        settleRead(
          analysisToken,
          async () => indexAnalyses(await api.projectAnalysisIndex(activeProjectId)),
          setAnalysisIndex,
          {},
        ),
        settleRead(readinessToken, () => api.analysisReadiness(activeProjectId), setReadiness, null),
        loadEvidence(evidenceToken, selectedIdRef.current),
      ]);
      if (assetsToken.session !== sessionRef.current) return;
      setLoading(false);
    },
    [begin, loadAssets, loadEvidence, settleRead],
  );

  useEffect(() => {
    // A new project is a new session: everything the previous one started stops
    // counting from here, whether or not it has answered yet.
    sessionRef.current += 1;
    // Cleared before the read starts rather than after it resolves: whatever is
    // in state belongs to the project that was open a frame ago.
    setAssets([]);
    setChronology(null);
    setAnalysisIndex({});
    setReadiness(null);
    setLoadError("");
    void loadAll(projectId);
    return () => {
      // Unmount, or a project change: nothing this session started may land.
      sessionRef.current += 1;
    };
  }, [loadAll, projectId]);

  // Per-clip detail is fetched on selection rather than for the whole project:
  // segment lists and OCR are large and only one clip is on screen.
  useEffect(() => {
    const token = begin("evidence");
    setTranscript(null);
    setObservation(null);
    if (!selectedId) return;
    void loadEvidence(token, selectedId);
    // Deliberately not keyed on `assets`: every note keystroke rewrites that
    // list, and this read is two IPC calls and two SQLite opens.
  }, [begin, loadEvidence, selectedId]);

  const reload = useCallback(async () => {
    await loadAll(projectIdRef.current);
  }, [loadAll]);

  const reloadAssets = useCallback(async () => {
    const token = begin("assets");
    const activeProjectId = projectIdRef.current;
    if (!activeProjectId) {
      setAssets([]);
      return;
    }
    await loadAssets(token, activeProjectId);
  }, [begin, loadAssets]);

  const reloadChronology = useCallback(async () => {
    const token = begin("chronology");
    const activeProjectId = projectIdRef.current;
    if (!activeProjectId) {
      setChronology(null);
      return;
    }
    await settleRead(token, () => chronologyApi.overview(activeProjectId), setChronology, null);
  }, [begin, settleRead]);

  const reloadReadiness = useCallback(async () => {
    const token = begin("readiness");
    const activeProjectId = projectIdRef.current;
    if (!activeProjectId) {
      setReadiness(null);
      return;
    }
    // Unlike the others this one rethrows: the analysis dialog is gated on
    // readiness, and its caller reports the failure rather than opening a
    // dialog that would silently understate the work.
    //
    // The rethrow is session-checked all the same. A caller reports what it
    // catches by calling `setError`, which is global, so an obsolete rejection
    // escaping here would put the abandoned project's failure on the current
    // one — the same defect as an obsolete success, and #28 rules out both.
    let next: AnalysisReadiness;
    try {
      next = await api.analysisReadiness(activeProjectId);
    } catch (reason) {
      if (!isCurrent(token)) return;
      throw reason;
    }
    if (!isCurrent(token)) return;
    setReadiness(next);
  }, [begin, isCurrent]);

  const reloadEvidence = useCallback(async () => {
    await loadEvidence(begin("evidence"), selectedIdRef.current);
  }, [begin, loadEvidence]);

  const applyAsset = useCallback((updated: Asset) => {
    setAssets((current) => sortAssets(current.map((asset) => (asset.id === updated.id ? updated : asset))));
  }, []);

  const patchAssets = useCallback((update: (current: Asset[]) => Asset[]) => {
    setAssets((current) => update(current));
  }, []);

  return {
    assets,
    chronology,
    analysisIndex,
    readiness,
    transcript,
    observation,
    loading,
    loadError,
    reload,
    reloadAssets,
    reloadChronology,
    reloadReadiness,
    reloadEvidence,
    applyAsset,
    patchAssets,
  };
}
