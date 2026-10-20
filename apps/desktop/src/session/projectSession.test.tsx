/**
 * The session owner, without the shell.
 *
 * The CUJ suite proves the invariant through the interface for the reads that
 * are visible there. Two things are easier to pin here: the chronology overview
 * (the harness serves one overview for every project, so a CUJ cannot tell A's
 * from B's), and the independence of the reload paths — a readiness reload
 * issued alongside a queue reload must not cancel it.
 */

import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useProjectSession } from "./projectSession";

const listAssets = vi.fn();
const assetTranscript = vi.fn();
const assetAnalysis = vi.fn();
const projectAnalysisIndex = vi.fn();
const analysisReadiness = vi.fn();
const overview = vi.fn();
const detectOfflineVolumes = vi.fn();

vi.mock("../api", () => ({
  api: {
    listAssets: (...args: unknown[]) => listAssets(...args),
    assetTranscript: (...args: unknown[]) => assetTranscript(...args),
    assetAnalysis: (...args: unknown[]) => assetAnalysis(...args),
    projectAnalysisIndex: (...args: unknown[]) => projectAnalysisIndex(...args),
    analysisReadiness: (...args: unknown[]) => analysisReadiness(...args),
  },
}));

vi.mock("../chronologyApi", () => ({
  chronologyApi: {
    overview: (...args: unknown[]) => overview(...args),
    detectOfflineVolumes: (...args: unknown[]) => detectOfflineVolumes(...args),
  },
}));

/** A promise the test decides the timing of. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function chronologyFor(projectId: string) {
  return { projectId, assets: [], phases: [], sequences: [], diagnostics: [] } as never;
}

beforeEach(() => {
  vi.clearAllMocks();
  listAssets.mockResolvedValue([]);
  assetTranscript.mockResolvedValue(null);
  assetAnalysis.mockResolvedValue(null);
  projectAnalysisIndex.mockResolvedValue([]);
  analysisReadiness.mockResolvedValue(null);
  overview.mockImplementation(async (projectId: string) => chronologyFor(projectId));
  detectOfflineVolumes.mockResolvedValue([]);
});

function renderSession(projectId: string) {
  const onError = vi.fn();
  const rendered = renderHook(
    ({ project }: { project: string }) =>
      useProjectSession({ projectId: project, selectedId: "", onError }),
    { initialProps: { project: projectId } },
  );
  return { ...rendered, onError };
}

describe("a chronology read that outlives its project", () => {
  it("cannot replace the overview of the project the user moved to", async () => {
    const held = deferred<unknown>();
    overview.mockImplementationOnce(() => held.promise);

    const session = renderSession("project_a");
    const { rerender, result } = session;
    await waitFor(() => expect(overview).toHaveBeenCalledWith("project_a"));

    rerender({ project: "project_b" });
    await waitFor(() => expect(result.current.chronology).toEqual(chronologyFor("project_b")));

    // Project A answers at last, into a session that has moved on.
    await act(async () => {
      held.resolve(chronologyFor("project_a"));
      await held.promise;
    });

    expect(result.current.chronology).toEqual(chronologyFor("project_b"));
  });
});

describe("reload paths", () => {
  it("do not cancel one another", async () => {
    const session = renderSession("project_a");
    const { result } = session;
    await waitFor(() => expect(listAssets).toHaveBeenCalledTimes(1));

    listAssets.mockResolvedValue([{ id: "asset_1" }]);
    analysisReadiness.mockResolvedValue({ projectId: "project_a", totalClips: 1, reachableOriginals: 1, unverifiedVideoClips: 0, quarantinedVideoClips: 0, unavailableVideoClips: 0 });

    // Issued together, as the app does after an import: each has to land.
    await act(async () => {
      await Promise.all([result.current.reloadAssets(), result.current.reloadReadiness()]);
    });

    expect(result.current.assets).toEqual([{ id: "asset_1" }]);
    expect(result.current.readiness).toEqual({ projectId: "project_a", totalClips: 1, reachableOriginals: 1, unverifiedVideoClips: 0, quarantinedVideoClips: 0, unavailableVideoClips: 0 });
  });

  it("report a failed queue read as retryable, and clear it when the retry works", async () => {
    listAssets.mockRejectedValueOnce(new Error("catalog unavailable"));
    const session = renderSession("project_a");
    const { onError, result } = session;

    await waitFor(() => expect(result.current.loadError).toContain("catalog unavailable"));
    expect(onError).toHaveBeenCalledWith(expect.stringContaining("catalog unavailable"));
    expect(result.current.assets).toEqual([]);

    listAssets.mockResolvedValue([{ id: "asset_1" }]);
    await act(async () => {
      await result.current.reload();
    });

    expect(result.current.loadError).toBe("");
    expect(result.current.assets).toEqual([{ id: "asset_1" }]);
  });

  it("drop a stale readiness rejection rather than reporting it against the current project", async () => {
    const { rerender, result, onError } = renderSession("project_a");
    await waitFor(() => expect(analysisReadiness).toHaveBeenCalledWith("project_a"));

    // Readiness is the one read that rethrows, so its rejection reaches a
    // caller whose `catch` writes the global error. It still must not do that
    // on behalf of a project the user has left.
    const held = deferred<never>();
    analysisReadiness.mockImplementationOnce(() => held.promise);
    let outcome = "pending";
    let settled!: Promise<void>;
    act(() => {
      settled = result.current.reloadReadiness().then(
        () => { outcome = "resolved"; },
        () => { outcome = "rejected"; },
      );
    });

    analysisReadiness.mockResolvedValue({ projectId: "project_b", totalClips: 2, reachableOriginals: 2, unverifiedVideoClips: 0, quarantinedVideoClips: 0, unavailableVideoClips: 0 });
    rerender({ project: "project_b" });
    await waitFor(() =>
      expect(result.current.readiness).toEqual({ projectId: "project_b", totalClips: 2, reachableOriginals: 2, unverifiedVideoClips: 0, quarantinedVideoClips: 0, unavailableVideoClips: 0 }),
    );

    await act(async () => {
      held.reject(new Error("readiness unavailable"));
      await settled;
    });

    expect(outcome).toBe("resolved");
    expect(result.current.readiness).toEqual({ projectId: "project_b", totalClips: 2, reachableOriginals: 2, unverifiedVideoClips: 0, quarantinedVideoClips: 0, unavailableVideoClips: 0 });
    expect(onError).not.toHaveBeenCalled();
  });

  it("still rethrow a readiness failure from the project the user is in", async () => {
    const { result } = renderSession("project_a");
    await waitFor(() => expect(analysisReadiness).toHaveBeenCalledWith("project_a"));

    analysisReadiness.mockRejectedValueOnce(new Error("readiness unavailable"));
    let caught: unknown = null;
    await act(async () => {
      await result.current.reloadReadiness().catch((reason) => {
        caught = reason;
      });
    });

    // The analysis dialog is gated on this read: a current failure has to reach
    // the caller rather than opening a dialog that understates the work.
    expect(String(caught)).toContain("readiness unavailable");
  });

  it("drop a stale queue rejection rather than blanking the current project", async () => {
    const held = deferred<never>();
    listAssets.mockImplementationOnce(() => held.promise);

    const { rerender, result, onError } = renderSession("project_a");
    await waitFor(() => expect(listAssets).toHaveBeenCalledWith("project_a"));

    listAssets.mockResolvedValue([{ id: "asset_b" }]);
    rerender({ project: "project_b" });
    await waitFor(() => expect(result.current.assets).toEqual([{ id: "asset_b" }]));

    await act(async () => {
      held.reject(new Error("catalog unavailable"));
      await held.promise.catch(() => {});
    });

    expect(result.current.assets).toEqual([{ id: "asset_b" }]);
    expect(result.current.loadError).toBe("");
    expect(onError).not.toHaveBeenCalled();
  });
});
