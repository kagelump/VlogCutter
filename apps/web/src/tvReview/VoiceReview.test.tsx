import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { BrowserProjectClient, ReviewSessionDetail } from "@rcdvlog/project-client";
import { RecordView, SessionDetailView, formatClock, type VoiceDeps } from "./VoiceReview";
import { MemoryOutbox } from "./recording/outbox";

const target = {
  reviewTargetId: "rtarget_one", projectId: "p", revisionId: "editrev_synthetic", viewedArtifactId: "preview", targetDigest: "a".repeat(64),
  durationSeconds: 6, cutCount: 3, youtube: { videoId: "synthetic01", declaredBy: "export", identityStatus: "declaredUnverified" },
  importedAt: "t", latestVerifiedAt: "t", readiness: { ready: true, freshness: "fresh", verificationAgeSeconds: 1, maxVerificationAgeSeconds: 10, availabilityState: "present", blockers: [] },
  movie: { blake3: "d".repeat(64), audioSampleRate: 48000 },
};

function fakeStream(): MediaStream {
  const track = { stop: vi.fn(), getSettings: () => ({ deviceId: "mic-1", channelCount: 1 }) };
  return { getAudioTracks: () => [track], getTracks: () => [track] } as unknown as MediaStream;
}

function deps(overrides: Partial<VoiceDeps> = {}): VoiceDeps {
  return {
    outbox: new MemoryOutbox(),
    getUserMedia: vi.fn().mockResolvedValue(fakeStream()),
    listMicrophones: vi.fn().mockResolvedValue([{ deviceId: "mic-1", label: "Desk microphone", kind: "audioinput" }]),
    startRecording: vi.fn(),
    ...overrides,
  };
}

const client = { getReviewTarget: vi.fn().mockResolvedValue(target) } as unknown as BrowserProjectClient;

test("capture never starts without explicit permission and consent", async () => {
  const recorder = { session: { sessionId: "rsession_1" }, cue: vi.fn(), stop: vi.fn().mockResolvedValue({ sessionId: "rsession_1" }), pageHidden: vi.fn() };
  const voice = deps({ startRecording: vi.fn().mockResolvedValue(recorder) });
  const navigate = vi.fn();
  render(<RecordView client={client} projectId="p" targetId="rtarget_one" navigate={navigate} deps={voice} />);
  expect(await screen.findByRole("heading", { name: "Record a TV review" })).toBeVisible();
  expect(voice.getUserMedia).not.toHaveBeenCalled();
  expect(screen.queryByRole("button", { name: "● Record" })).toBeDisabled();
  await userEvent.click(screen.getByRole("button", { name: "Allow microphone" }));
  expect(await screen.findByRole("option", { name: "Desk microphone" })).toBeInTheDocument();
  const constraints = vi.mocked(voice.getUserMedia).mock.calls[0][0] as { audio: MediaTrackConstraints };
  expect(constraints.audio).toMatchObject({ echoCancellation: false, noiseSuppression: false, autoGainControl: false });
  expect(screen.getByRole("button", { name: "● Record" })).toBeDisabled();
  await userEvent.click(screen.getByLabelText(/Everyone present knows/));
  await userEvent.click(screen.getByLabelText(/Another person will speak/));
  await userEvent.click(screen.getByRole("button", { name: "● Record" }));
  expect(await screen.findByRole("status")).toHaveTextContent("Recording");
  expect(vi.mocked(voice.startRecording).mock.calls[0][0]).toMatchObject({ projectId: "p", additionalSpeakersDisclosed: true });
  await userEvent.keyboard("m");
  expect(recorder.cue).toHaveBeenCalledWith(0);
  await userEvent.click(screen.getByRole("button", { name: "Mark video start" }));
  expect(recorder.cue).toHaveBeenCalledTimes(2);
  await userEvent.click(screen.getByRole("button", { name: "■ Stop" }));
  await waitFor(() => expect(navigate).toHaveBeenCalledWith("/tv-review/p/rtarget_one/sessions/rsession_1"));
});

test("a refused microphone explains how to retry", async () => {
  const voice = deps({ getUserMedia: vi.fn().mockRejectedValueOnce(new DOMException("denied", "NotAllowedError")).mockResolvedValue(fakeStream()) });
  render(<RecordView client={client} projectId="p" targetId="rtarget_one" navigate={vi.fn()} deps={voice} />);
  await userEvent.click(await screen.findByRole("button", { name: "Allow microphone" }));
  expect(await screen.findByText(/Microphone access was refused/)).toBeVisible();
  await userEvent.click(screen.getByRole("button", { name: "Try microphone again" }));
  expect(await screen.findByLabelText("Input device")).toBeVisible();
});

test("a stopped recording whose upload cannot finish stays queued, not finalized", async () => {
  const recorder = { session: { sessionId: "rsession_q" }, cue: vi.fn(), stop: vi.fn().mockResolvedValue(null), pageHidden: vi.fn() };
  const navigate = vi.fn();
  render(<RecordView client={client} projectId="p" targetId="rtarget_one" navigate={navigate} deps={deps({ startRecording: vi.fn().mockResolvedValue(recorder) })} />);
  await userEvent.click(await screen.findByRole("button", { name: "Allow microphone" }));
  await userEvent.click(await screen.findByLabelText(/Everyone present knows/));
  await userEvent.click(screen.getByRole("button", { name: "● Record" }));
  await userEvent.click(await screen.findByRole("button", { name: "■ Stop" }));
  expect(await screen.findByText(/still buffered in this browser/)).toBeVisible();
  expect(navigate).not.toHaveBeenCalled();
});

test("incomplete sessions show their gaps and offer no padded download", async () => {
  const session = {
    sessionId: "rsession_1", projectId: "p", reviewTargetId: "rtarget_one", targetDigest: "a".repeat(64), revisionId: "editrev_synthetic", viewedArtifactId: "x",
    state: "incomplete", abandoned: false, endReason: "deviceLost", sampleRate: 48000, channels: 1, encoding: "pcm_s16le", createdAt: "t", finalizedAt: "t2", deletedAt: null,
    receivedSeconds: 4, manualCueCount: 1,
    consent: { allPresentAcknowledged: true, additionalSpeakersDisclosed: false },
    capture: { echoCancellation: false, noiseSuppression: false, autoGainControl: false, clock: "audioWorkletCurrentFrame", baseLatencySeconds: null, userAgentFamily: "chromium" },
    createdBy: { actorType: "browser_session", actorId: "s" }, clientStartedPerformanceMs: null, recordingExpiresAt: "t",
    coverage: { receivedFrames: 192000, receivedBytes: 384000, chunkCount: 2, highestSequence: 2, missingSequences: [1], gaps: [{ startFrame: 96000, endFrame: 192000 }], finalFrameCount: 288000, receivedSeconds: 4 },
    chunks: [], events: [{ eventId: "c", kind: "manualSyncCue", audioFrame: 48000, audioSeconds: 1, performanceMs: 1, declaredMovieSeconds: 0, serverReceivedAt: "t", detail: null }],
    limits: { maxChunkBytes: 1, maxSessionBytes: 1, recordingLifetimeHours: 12 },
    storage: { location: "Private server data directory.", retention: "Kept until you delete it.", backup: "Not in backups." },
  } as unknown as ReviewSessionDetail;
  const deleted = { ...session, state: "deleted", deletedAt: "t3" };
  const sessionClient = {
    getReviewSession: vi.fn().mockResolvedValue(session),
    deleteReviewSession: vi.fn().mockResolvedValue(deleted),
    reviewSessionAudioUrl: vi.fn().mockReturnValue("/audio.wav"),
  } as unknown as BrowserProjectClient;
  render(<SessionDetailView client={sessionClient} projectId="p" targetId="rtarget_one" sessionId="rsession_1" navigate={vi.fn()} />);
  expect(await screen.findByText("Incomplete · gaps recorded")).toBeVisible();
  expect(screen.getByText(/0:02–0:04 \(no audio exists/)).toBeVisible();
  expect(screen.getByText(/reviewer declaration, not measured sync/)).toBeVisible();
  expect(screen.queryByRole("link", { name: "Download WAV" })).toBeNull();
  await userEvent.click(screen.getByRole("button", { name: "Delete recording…" }));
  await userEvent.click(screen.getByRole("button", { name: "Delete audio permanently" }));
  expect(await screen.findByText(/Audio deleted t3/)).toBeVisible();
});

test("formats the recording clock", () => {
  expect(formatClock(0)).toBe("0:00");
  expect(formatClock(65)).toBe("1:05");
  expect(formatClock(3725)).toBe("1:02:05");
});
