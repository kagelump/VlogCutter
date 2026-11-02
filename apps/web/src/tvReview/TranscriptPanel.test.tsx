import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { BrowserProjectClient, ReviewTranscriptionRun } from "@rcdvlog/project-client";
import { TranscriptPanel } from "./TranscriptPanel";

const run: ReviewTranscriptionRun = {
  schema: "review-transcript.v1", runId: "rtrans_1", projectId: "p", sessionId: "s", reviewTargetId: "t", revisionId: "r",
  backend: "openrouter", provider: "openrouter", model: "openai/whisper-1", recipe: "x",
  settings: { backend: "openrouter" }, audioFingerprint: "f", sampleRate: 48000, state: "partial", errorCode: "review_transcription_budget_exhausted",
  attemptCount: 1, jobId: "j", createdAt: "t", updatedAt: "t", createdBy: { actorType: "browser_session", actorId: "x" },
  segments: [{ segmentIndex: 1, startFrame: 0, endFrame: 1, startSeconds: 600, endSeconds: 900, state: "failed", speechSeconds: 0, windowCount: 0, errorCode: "review_transcription_budget_exhausted" }],
  utterances: [
    { utteranceId: "a", segmentIndex: 0, windowIndex: 0, startFrame: 0, endFrame: 1, startSeconds: 1, endSeconds: 3.5, text: "这个镜头太长了", language: "zh", script: "hanSimplified", displayText: "這個鏡頭太長了", displayProjection: "openccS2tw", speaker: "unknown", speakerEvidence: "diarizationUnavailable", meanTokenProbability: null, flags: ["probabilityEvidenceUnavailable"], suppressed: false },
    { utteranceId: "b", segmentIndex: 0, windowIndex: 1, startFrame: 0, endFrame: 1, startSeconds: 4, endSeconds: 6, text: "thank you thank you", language: "en", script: "latin", displayText: "thank you thank you", displayProjection: null, speaker: "unknown", speakerEvidence: "diarizationUnavailable", meanTokenProbability: 0.1, flags: ["qualityGate:repetition"], suppressed: true },
  ],
  recordingGaps: [], spend: { authorizedMaxUsd: 0.01, reservedUsd: 0, knownCostUsd: 0.002, unknownCostWindows: 0, dispatchedWindows: 2, cachedWindows: 2 },
  diarization: { status: "unavailable", reason: "No diarization backend is configured." }, limitations: [],
};

function client(overrides: Partial<BrowserProjectClient> = {}) {
  return {
    listReviewTranscriptions: vi.fn().mockResolvedValue({ items: [] }),
    preflightReviewTranscription: vi.fn(),
    startReviewTranscription: vi.fn(),
    cancelReviewTranscription: vi.fn(),
    ...overrides,
  } as unknown as BrowserProjectClient;
}

test("cloud transcription needs explicit upload consent and a ceiling, and quotes worst-case cost", async () => {
  const preflight = vi.fn()
    .mockResolvedValueOnce({ ready: false, blockers: ["review_transcription_cloud_not_authorized"], warnings: ["diarization_unavailable"], backend: "openrouter", provider: "openrouter", model: "openai/whisper-1", audioFingerprint: "f", estimate: { receivedAudioSeconds: 20, segmentCount: 1, maxWindows: 1, priceUsdPerSecond: 0.0001, maxCostUsd: 0.003, billingNote: null }, existingRunId: null })
    .mockResolvedValueOnce({ ready: true, blockers: [], warnings: [], backend: "openrouter", provider: "openrouter", model: "openai/whisper-1", audioFingerprint: "f", estimate: { receivedAudioSeconds: 20, segmentCount: 1, maxWindows: 1, priceUsdPerSecond: 0.0001, maxCostUsd: 0.003, billingNote: null }, existingRunId: null });
  const start = vi.fn().mockResolvedValue(run);
  const mock = client({ preflightReviewTranscription: preflight, startReviewTranscription: start });
  render(<TranscriptPanel client={mock} projectId="p" sessionId="s" sessionState="completed" />);
  await userEvent.click(screen.getByLabelText(/OpenRouter cloud/));
  await userEvent.click(screen.getByRole("button", { name: "Check" }));
  expect(await screen.findByText(/may be sent to the selected provider/)).toBeVisible();
  expect(screen.getByText(/Worst-case cost \$0.0030/)).toBeVisible();
  expect(screen.queryByRole("button", { name: "Transcribe" })).toBeNull();
  await userEvent.click(screen.getByLabelText(/Send this private recording/));
  await userEvent.click(screen.getByRole("button", { name: "Check" }));
  await userEvent.click(await screen.findByRole("button", { name: "Transcribe" }));
  await waitFor(() => expect(start).toHaveBeenCalledWith("p", "s", { backend: "openrouter", model: "openai/whisper-1", authorizedMaxCostUsd: 0.5, cloudUploadAcknowledged: true, zeroDataRetention: true }));
});

test("transcript shows Traditional display with raw evidence, unknown speakers and quality-gated text", async () => {
  render(<TranscriptPanel client={client({ listReviewTranscriptions: vi.fn().mockResolvedValue({ items: [run] }) })} projectId="p" sessionId="s" sessionState="completed" />);
  expect(await screen.findByText("這個鏡頭太長了")).toBeVisible();
  expect(screen.getByText(/raw: 这个镜头太长了/)).toBeVisible();
  expect(screen.getAllByText("Speaker ?")).toHaveLength(2);
  expect(screen.getByText(/Quality-gated/)).toBeVisible();
  expect(screen.getByText("Partially transcribed")).toBeVisible();
  expect(screen.getAllByText(/spend ceiling was reached/).length).toBeGreaterThan(0);
  expect(screen.getByText(/10:00.0–15:00.0 not transcribed/)).toBeVisible();
  await userEvent.click(screen.getByLabelText("Show raw text"));
  expect(screen.getByText("这个镜头太长了")).toBeVisible();
});

test("an unfinalized recording offers no transcription", () => {
  render(<TranscriptPanel client={client()} projectId="p" sessionId="s" sessionState="recording" />);
  expect(screen.getByText(/once the recording is finalized/)).toBeVisible();
});
