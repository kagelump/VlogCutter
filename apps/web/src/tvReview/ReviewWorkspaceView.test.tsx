import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ProjectClientError, type BrowserProjectClient, type ReviewWorkspace } from "@rcdvlog/project-client";
import { ReviewWorkspaceView } from "./ReviewWorkspaceView";

const machine = { utteranceId: "s0w0u0", segmentIndex: 0, windowIndex: 0, startFrame: 48000, endFrame: 96000, startSeconds: 1, endSeconds: 2, text: "这个太长", language: "zh", script: "hanSimplified", displayText: "這個太長", displayProjection: "openccS2tw", speaker: "unknown", speakerEvidence: "diarizationUnavailable", meanTokenProbability: 0.8, flags: [], suppressed: false };
const workspace: ReviewWorkspace = {
  runId: "r", sessionId: "s", projectId: "p", reviewTargetId: "t", revisionId: "editrev", viewedArtifactId: "preview", targetDigest: "d",
  sync: { status: "mapped", source: "sharedSync", note: "Movie time comes from the shared synchronizer.", calibratedClocks: 1, anchors: 2, boundaries: 0, diagnostics: [],
    policy: { minimumConfidence: 0.8, maxAnchorGapSeconds: 15, maxExtrapolationSeconds: 0, maxDriftPpm: 2000, residualToleranceSeconds: 0.1, unobservedUncertaintyPpm: 5000 } },
  cuts: [{ operationId: "op_a", position: 0, assetId: "a", movieInSeconds: 0, movieOutSeconds: 2 }, { operationId: "op_b", position: 1, assetId: "a", movieInSeconds: 2, movieOutSeconds: 4 }],
  utterances: [{ machine, text: machine.text, displayText: machine.displayText, speaker: "unknown", language: "zh", excluded: false, movieTime: { movieSeconds: 0.5, uncertaintySeconds: 0.75, source: "sharedSync:estimated", evidenceIds: ["c", "c2"] },
    sync: { state: "mixed", uncoveredFrames: 0, slices: [
      { recording: { clockId: "mic_epoch_0", startSample: 48000, endSample: 72000 }, state: "estimated", candidates: [{ startPts: 45000, endPts: 90000, lowerPts: 0, upperPts: 150000, uncertaintyTicks: 67500, confidence: 0.9, evidenceIds: ["c", "c2"], measuredDriftPpm: 0 }], diagnostics: [], candidateOperationIds: ["op_a"] },
      { recording: { clockId: "mic_epoch_0", startSample: 72000, endSample: 96000 }, state: "unknown", candidates: [], diagnostics: [], candidateOperationIds: [] }] },
    candidateOperationIds: ["op_a"], referenceHint: null, corrections: [] }],
  discussions: [{ discussionId: "d1", title: "這個太長", groupingSource: "heuristic", state: "open", utteranceIds: ["s0w0u0"], startRecordingSeconds: 1, endRecordingSeconds: 2, languages: ["zh"], speakers: ["unknown"], candidateOperationIds: ["op_a"], targetOperationId: null, targetSource: null, unanchored: false, updatedAt: "t",
    items: [{ itemId: "i1", kind: "candidateEdit", authorKind: "agent", provisional: true, actor: { actorType: "device_credential", actorId: "x" }, text: "Agent thinks: trim it", speaker: null, quoteUtteranceIds: [], category: null, change: null, decision: null, refersToItemId: null, status: "open", createdAt: "t", canonicalNoteId: null }] }],
  coverage: { audiovisualReviewAttested: false, note: "A recording does not attest continuous review." },
};

function client(overrides: Partial<BrowserProjectClient> = {}) {
  return {
    getReviewWorkspace: vi.fn().mockResolvedValue(workspace),
    reviewReportUrl: vi.fn().mockReturnValue("/report"),
    correctReviewUtterance: vi.fn().mockResolvedValue(workspace),
    updateReviewDiscussion: vi.fn().mockResolvedValue(workspace),
    addReviewDiscussionItem: vi.fn(),
    proposeReviewDiscussions: vi.fn(),
    getReviewSession: vi.fn().mockResolvedValue({ state: "incomplete", sampleRate: 48000, coverage: { storedRanges: [{ startFrame: 0, endFrame: 480000 }] } }),
    reviewSessionSnippetUrl: vi.fn((_p: string, _s: string, a: number, b: number) => `/snippet.wav?startFrame=${a}&endFrame=${b}`),
    ...overrides,
  } as unknown as BrowserProjectClient;
}

test("shows clocks, candidates and provisional agent suggestions, and records speaker corrections", async () => {
  const mock = client();
  render(<ReviewWorkspaceView client={mock} projectId="p" targetId="t" sessionId="s" runId="r" navigate={vi.fn()} />);
  expect(await screen.findByRole("heading", { name: "Editorial review" })).toBeVisible();
  expect(screen.getByText(/movie 0:00.5 \(±0.75 s, estimated; later part unknown or conflicting\)/)).toBeVisible();
  expect(screen.getByText("Movie time: shared sync")).toBeVisible();
  expect(screen.getByText("Agent suggestion · provisional")).toBeVisible();
  expect(screen.getByText(/does not attest continuous review/)).toBeVisible();
  expect(screen.getByRole("option", { name: /★ #1 op_a/ })).toBeInTheDocument();
  await userEvent.selectOptions(screen.getByLabelText("Speaker for s0w0u0"), "B");
  await waitFor(() => expect(mock.correctReviewUtterance).toHaveBeenCalledWith("p", "r", "s0w0u0", "speaker", "B"));
});

test("a refused acceptance is explained by its stable code", async () => {
  const mock = client({ addReviewDiscussionItem: vi.fn().mockRejectedValue(new ProjectClientError("server_error", "x", "review_target_unconfirmed", null, 409)) });
  render(<ReviewWorkspaceView client={mock} projectId="p" targetId="t" sessionId="s" runId="r" navigate={vi.fn()} />);
  await screen.findByRole("heading", { name: "Editorial review" });
  await userEvent.selectOptions(screen.getByLabelText("Item kind"), "decision");
  await userEvent.selectOptions(screen.getByLabelText("Refers to"), "i1");
  await userEvent.click(screen.getByRole("button", { name: "Add" }));
  expect(await screen.findByText(/Confirm which cut this discussion is about/)).toBeVisible();
  expect(vi.mocked(mock.addReviewDiscussionItem).mock.calls[0][2]).toMatchObject({ kind: "decision", authorKind: "human", decision: "accepted", refersToItemId: "i1" });
});

test("each stored utterance can be heard in a bounded snippet with a little context", async () => {
  const mock = client();
  render(<ReviewWorkspaceView client={mock} projectId="p" targetId="t" sessionId="s" runId="r" navigate={vi.fn()} />);
  await userEvent.click(await screen.findByRole("button", { name: "Listen to utterance at 0:01.0" }));
  expect(screen.getByLabelText("Recording utterance at 0:01.0")).toHaveAttribute("src", "/snippet.wav?startFrame=24000&endFrame=120000");
});

test("an utterance whose audio was never stored offers nothing to play", async () => {
  const mock = client({ getReviewSession: vi.fn().mockResolvedValue({ state: "incomplete", sampleRate: 48000, coverage: { storedRanges: [{ startFrame: 200000, endFrame: 480000 }] } }) });
  render(<ReviewWorkspaceView client={mock} projectId="p" targetId="t" sessionId="s" runId="r" navigate={vi.fn()} />);
  expect(await screen.findByRole("heading", { name: "Editorial review" })).toBeVisible();
  await waitFor(() => expect(mock.getReviewSession).toHaveBeenCalled());
  expect(screen.queryByRole("button", { name: /Listen to utterance/ })).toBeNull();
});
