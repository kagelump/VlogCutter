import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReviewTargetPreflight } from "@rcdvlog/project-client";
import { memoryVoice, renderApp } from "../../test/render";
import { run, session, target, workspace } from "./fixtures";

const handoff = { schemaVersion: 1, targetDigest: "x", target: {}, verification: {} };
const preview: NonNullable<ReviewTargetPreflight["preview"]> = {
  contract: "reviewTargetV1", projectId: "p1", revisionId: "editrev_synthetic", viewedArtifactId: "preview_synthetic", targetDigest: "a".repeat(64),
  movieBlake3: "d".repeat(64), durationSeconds: 6.02, cutCount: 2, youtubeVideoId: "synthetic01", verifiedAt: "2026-11-02T00:00:00Z",
};

function preflight(overrides: Partial<ReviewTargetPreflight>): ReviewTargetPreflight {
  return { outcome: "new", importable: true, existingReviewTargetId: null, conflictingReviewTargetIds: [], maxVerificationAgeSeconds: 604800, preview, checks: [], ...overrides };
}

const clipAsset = {
  projectAssetId: "asset_repeated", filename: "C0042.MP4", camera: "Sony", mediaObjectId: null, mediaVerificationState: "unlinked",
  removed: false, rotationDegrees: 0, labels: ["penguin", "ice", "ship"], activeNote: null, description: "Penguins on blue ice", durationSeconds: 9, bestTime: null,
};

const targetsClient = {
  listReviewTargets: vi.fn().mockResolvedValue({ items: [], nextBefore: null }),
  getReviewTarget: vi.fn().mockResolvedValue(target),
  listReviewSessions: vi.fn().mockResolvedValue({ items: [], nextBefore: null }),
  listProjectAssets: vi.fn().mockResolvedValue({ items: [clipAsset], nextCursor: null }),
  listReviewTranscriptions: vi.fn().mockResolvedValue({ items: [] }),
};

async function openImport() {
  await userEvent.click((await screen.findAllByRole("button", { name: "Import cut" }))[0]);
  return screen.findByRole("dialog", { name: "Import a cut" });
}

test("importing checks the export first and opens the new cut", async () => {
  const { client, router } = renderApp("/projects/p1/reviews", {
    ...targetsClient,
    preflightReviewTarget: vi.fn().mockResolvedValue(preflight({ checks: [{ name: "youtube", status: "warn", code: null, detail: "declared, not verified" }] })),
    importReviewTarget: vi.fn().mockResolvedValue({ outcome: "created", reviewTarget: target }),
  });
  const dialog = await openImport();
  await userEvent.click(within(dialog).getByLabelText("Export JSON"));
  // The CLI envelope is accepted; only its data member is sent.
  await userEvent.paste(JSON.stringify({ ok: true, command: "edit-plan.review-target-export", data: handoff }));
  await userEvent.type(within(dialog).getByLabelText("Exported from"), "Travel Mac");
  expect(within(dialog).queryByRole("button", { name: "Import" })).toBeNull();
  await userEvent.click(within(dialog).getByRole("button", { name: "Check" }));
  expect(await within(dialog).findByText("Ready to import.")).toBeVisible();
  expect(client.preflightReviewTarget).toHaveBeenCalledWith("p1", { handoff, declaredIssuer: "Travel Mac" });
  expect(within(dialog).getByText(/declared, not verified/)).toBeVisible();
  await userEvent.click(within(dialog).getByRole("button", { name: "Import" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/projects/p1/reviews/rtarget_one"));
  expect(await screen.findByRole("heading", { name: /Cut syntheti/ })).toBeVisible();
});

test("a failing check is explained and nothing can be imported", async () => {
  renderApp("/projects/p1/reviews", {
    ...targetsClient,
    preflightReviewTarget: vi.fn().mockResolvedValue(preflight({
      outcome: "blocked", importable: false, preview: null,
      checks: [{ name: "targetDigest", status: "fail", code: "review_target_digest_mismatch", detail: "raw server prose" }],
    })),
  });
  const dialog = await openImport();
  await userEvent.click(within(dialog).getByLabelText("Export JSON"));
  await userEvent.paste(JSON.stringify(handoff));
  await userEvent.click(within(dialog).getByRole("button", { name: "Check" }));
  expect(await within(dialog).findByText(/edited after export/)).toBeVisible();
  expect(within(dialog).queryByText("raw server prose")).toBeNull();
  expect(within(dialog).queryByRole("button", { name: "Import" })).toBeNull();
});

test("a historical export needs explicit acknowledgement and uses the historical endpoint", async () => {
  const { client } = renderApp("/projects/p1/reviews", {
    ...targetsClient,
    preflightHistoricalReviewTarget: vi.fn().mockResolvedValue(preflight({ preview: { ...preview, contract: "historicalRenderedArtifactReviewV1" } })),
  });
  const dialog = await openImport();
  await userEvent.click(within(dialog).getByLabelText("Export JSON"));
  await userEvent.paste(JSON.stringify({ ...handoff, purpose: "historicalRenderedArtifactReview" }));
  expect(within(dialog).getByText("Historical review.")).toBeVisible();
  expect(within(dialog).getByRole("button", { name: "Check" })).toBeDisabled();
  await userEvent.click(within(dialog).getByLabelText(/covers the rendered movie only/));
  await userEvent.click(within(dialog).getByRole("button", { name: "Check" }));
  expect(await within(dialog).findByText("Historical (integrity only)")).toBeVisible();
  expect(client.preflightHistoricalReviewTarget).toHaveBeenCalledWith("p1", expect.objectContaining({ acknowledgement: "integrityOnlyCurrentRenderabilityNotEstablished" }));
});

test("a cut page links to YouTube only when the orchestrator provided a video", async () => {
  renderApp("/projects/p1/reviews/rtarget_one", targetsClient);
  expect(await screen.findByRole("heading", { name: /Cut syntheti/ })).toBeVisible();
  expect(screen.queryByRole("link", { name: "YouTube" })).toBeNull();
  expect(screen.queryByText(/YouTube/)).toBeNull();
});

test("the YouTube button opens the declared video", async () => {
  const linked = { ...target, youtube: { videoId: "synthetic02", declaredBy: "export" as const, identityStatus: "declaredUnverified" as const } };
  renderApp("/projects/p1/reviews/rtarget_one", { ...targetsClient, getReviewTarget: vi.fn().mockResolvedValue(linked) });
  expect(await screen.findByRole("link", { name: "YouTube" })).toHaveAttribute("href", "https://www.youtube.com/watch?v=synthetic02");
  expect(screen.queryByRole("button", { name: /Remove/ })).toBeNull();
});

test("shots show analysis descriptions and thumbnails, filter by label, and open the clip", async () => {
  const { router } = renderApp("/projects/p1/reviews/rtarget_one", targetsClient);
  const shots = await screen.findByRole("region", { name: /Shots/ });
  const cards = within(shots).getAllByRole("link");
  expect(cards).toHaveLength(2);
  expect(cards[0]).toHaveTextContent("2.0s");
  expect(await within(cards[0]).findByText("Penguins on blue ice")).toBeVisible();
  expect(cards[0].querySelector("img")).toHaveAttribute("src", "/api/v1/projects/p1/assets/asset_repeated/thumbnail");
  await userEvent.type(within(shots).getByLabelText("Filter shots"), "walrus");
  expect(await within(shots).findByText(/No shots match/)).toBeVisible();
  await userEvent.clear(within(shots).getByLabelText("Filter shots"));
  await userEvent.type(within(shots).getByLabelText("Filter shots"), "ICE");
  await waitFor(() => expect(within(shots).getAllByRole("link")).toHaveLength(2));
  await userEvent.click(within(shots).getAllByRole("link")[1]);
  await waitFor(() => expect(router.state.location.search).toBe("?clip=asset_repeated"));
});

test("a recording without a transcript can be transcribed from the cut page", async () => {
  const completed = { ...session, state: "completed" as const };
  const listReviewTranscriptions = vi.fn()
    .mockResolvedValueOnce({ items: [] })
    .mockResolvedValue({ items: [run({ state: "running", utterances: [] })] });
  const { client } = renderApp("/projects/p1/reviews/rtarget_one", {
    ...targetsClient,
    listReviewSessions: vi.fn().mockResolvedValue({ items: [completed], nextBefore: null }),
    listReviewTranscriptions,
    preflightReviewTranscription: vi.fn().mockResolvedValue({ ready: true, blockers: [], warnings: [], backend: "local", provider: "local", model: "m", audioFingerprint: "f", estimate: { receivedAudioSeconds: 4, segmentCount: 1, maxWindows: 1, priceUsdPerSecond: null, maxCostUsd: null, billingNote: null }, existingRunId: null }),
    startReviewTranscription: vi.fn().mockResolvedValue(run({ state: "queued", utterances: [] })),
  });
  const recordings = await screen.findByRole("region", { name: "Recordings" });
  await userEvent.click(await within(recordings).findByRole("button", { name: "Transcribe" }));
  const dialog = await screen.findByRole("dialog", { name: "Transcribe recording" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Check" }));
  await userEvent.click(await within(dialog).findByRole("button", { name: "Transcribe" }));
  expect(client.startReviewTranscription).toHaveBeenCalledWith("p1", "rsession_1", { backend: "local" });
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(await within(recordings).findByText("Transcribing…")).toBeVisible();
});

test("a transcribed recording goes straight to its review notes", async () => {
  renderApp("/projects/p1/reviews/rtarget_one", {
    ...targetsClient,
    listReviewSessions: vi.fn().mockResolvedValue({ items: [{ ...session, state: "completed" }], nextBefore: null }),
    listReviewTranscriptions: vi.fn().mockResolvedValue({ items: [run()] }),
  });
  const recordings = await screen.findByRole("region", { name: "Recordings" });
  expect(await within(recordings).findByRole("link", { name: "Review notes" })).toHaveAttribute("href", "/projects/p1/reviews/rtarget_one/sessions/rsession_1/runs/rtrans_local");
  expect(within(recordings).getByText(/1 transcript line/)).toBeVisible();
  expect(within(recordings).queryByRole("button", { name: "Transcribe" })).toBeNull();
});

/* ---------- Recording ---------- */

function fakeStream(): MediaStream {
  const track = { stop: vi.fn(), getSettings: () => ({ deviceId: "mic-1", channelCount: 1 }) };
  return { getAudioTracks: () => [track], getTracks: () => [track] } as unknown as MediaStream;
}

function micDeps(recorder: object, overrides = {}) {
  return memoryVoice({
    getUserMedia: vi.fn().mockResolvedValue(fakeStream()),
    listMicrophones: vi.fn().mockResolvedValue([{ deviceId: "mic-1", label: "Desk microphone", kind: "audioinput" }]),
    startRecording: vi.fn().mockResolvedValue(recorder),
    ...overrides,
  });
}

test("recording needs microphone permission and consent, raw capture, and lands on the session", async () => {
  const recorder = { session: { sessionId: "rsession_1" }, cue: vi.fn(), stop: vi.fn().mockResolvedValue({ sessionId: "rsession_1" }), pageHidden: vi.fn() };
  const voice = micDeps(recorder);
  const { router } = renderApp("/projects/p1/reviews/rtarget_one/record", { ...targetsClient, getReviewSession: vi.fn().mockResolvedValue({ ...session, state: "recording" }), listReviewTranscriptions: vi.fn().mockResolvedValue({ items: [] }) }, voice);
  expect(await screen.findByRole("heading", { name: "Record review" })).toBeVisible();
  expect(voice.getUserMedia).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "Record" })).toBeDisabled();
  await userEvent.click(screen.getByRole("button", { name: "Allow microphone" }));
  expect(await screen.findByRole("option", { name: "Desk microphone" })).toBeInTheDocument();
  const constraints = vi.mocked(voice.getUserMedia).mock.calls[0][0] as { audio: MediaTrackConstraints };
  expect(constraints.audio).toMatchObject({ echoCancellation: false, noiseSuppression: false, autoGainControl: false });
  expect(screen.getByRole("button", { name: "Record" })).toBeDisabled();
  await userEvent.click(screen.getByLabelText(/Everyone here knows/));
  await userEvent.click(screen.getByLabelText(/Someone else will talk/));
  await userEvent.click(screen.getByRole("button", { name: "Record" }));
  expect(await screen.findByText(/^Recording/)).toBeVisible();
  expect(vi.mocked(voice.startRecording).mock.calls[0][0]).toMatchObject({ projectId: "p1", additionalSpeakersDisclosed: true });
  await userEvent.keyboard("m");
  expect(recorder.cue).toHaveBeenCalledWith(0);
  await userEvent.click(screen.getByRole("button", { name: "Mark start" }));
  expect(recorder.cue).toHaveBeenCalledTimes(2);
  await userEvent.click(screen.getByRole("button", { name: "Stop" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/projects/p1/reviews/rtarget_one/sessions/rsession_1"));
});

test("a refused microphone explains how to retry", async () => {
  const voice = micDeps({}, { getUserMedia: vi.fn().mockRejectedValueOnce(new DOMException("denied", "NotAllowedError")).mockResolvedValue(fakeStream()) });
  renderApp("/projects/p1/reviews/rtarget_one/record", targetsClient, voice);
  await userEvent.click(await screen.findByRole("button", { name: "Allow microphone" }));
  expect(await screen.findByText(/Microphone access is blocked/)).toBeVisible();
  await userEvent.click(screen.getByRole("button", { name: "Try again" }));
  expect(await screen.findByLabelText("Input")).toBeVisible();
});

test("a stopped recording whose upload can't finish stays on the page with recovery guidance", async () => {
  const recorder = { session: { sessionId: "rsession_q" }, cue: vi.fn(), stop: vi.fn().mockResolvedValue(null), pageHidden: vi.fn() };
  const { router } = renderApp("/projects/p1/reviews/rtarget_one/record", targetsClient, micDeps(recorder));
  await userEvent.click(await screen.findByRole("button", { name: "Allow microphone" }));
  await userEvent.click(await screen.findByLabelText(/Everyone here knows/));
  await userEvent.click(screen.getByRole("button", { name: "Record" }));
  await userEvent.click(await screen.findByRole("button", { name: "Stop" }));
  expect(await screen.findByText("Recording stopped, upload unfinished.")).toBeVisible();
  expect(router.state.location.pathname).toBe("/projects/p1/reviews/rtarget_one/record");
});

/* ---------- Session & transcript ---------- */

test("an incomplete session shows its gaps, offers no download, and can delete its audio", async () => {
  const { client } = renderApp("/projects/p1/reviews/rtarget_one/sessions/rsession_1", {
    getReviewSession: vi.fn().mockResolvedValue(session),
    deleteReviewSession: vi.fn().mockResolvedValue({ ...session, state: "deleted", deletedAt: "2026-11-03T00:00:00Z" }),
    reviewSessionAudioUrl: vi.fn().mockReturnValue("/audio.wav"),
    listReviewTranscriptions: vi.fn().mockResolvedValue({ items: [] }),
    listReviewSessions: vi.fn().mockResolvedValue({ items: [], nextBefore: null }),
  });
  expect(await screen.findByText("Incomplete")).toBeVisible();
  expect(screen.queryByRole("link", { name: "Download WAV" })).toBeNull();
  await userEvent.click(screen.getByText("Recording details"));
  expect(screen.getByText("0:02–0:04")).toBeVisible();
  expect(screen.getByText(/your mark, not measured sync/)).toBeVisible();
  await userEvent.click(screen.getByRole("button", { name: "Delete audio" }));
  const confirm = await screen.findByRole("dialog", { name: /Delete this recording/ });
  await userEvent.click(within(confirm).getByRole("button", { name: "Delete audio" }));
  expect(await screen.findByText(/Audio deleted/)).toBeVisible();
  expect(client.deleteReviewSession).toHaveBeenCalledWith("p1", "rsession_1");
});

test("the transcript shows the newest usable run even when a later attempt failed", async () => {
  const failed = run({ runId: "rtrans_cloud", state: "failed", provider: "openrouter", model: "openai/whisper-1", errorCode: "review_transcription_failed", utterances: [], createdAt: "2026-11-02T14:17:00Z" });
  const { router } = renderApp("/projects/p1/reviews/rtarget_one/sessions/rsession_1", {
    getReviewSession: vi.fn().mockResolvedValue({ ...session, state: "completed" }),
    reviewSessionAudioUrl: vi.fn().mockReturnValue("/audio.wav"),
    listReviewTranscriptions: vi.fn().mockResolvedValue({ items: [failed, run()] }),
    getReviewWorkspace: vi.fn().mockResolvedValue(workspace()),
  });
  expect(await screen.findByText("I think I like the frame")).toBeVisible();
  expect(screen.getByRole("link", { name: "Download WAV" })).toHaveAttribute("href", "/audio.wav");
  await userEvent.click(screen.getByText("All attempts"));
  await userEvent.click(screen.getByRole("button", { name: /openrouter/ }));
  expect(await screen.findByText(/^openrouter · openai\/whisper-1 ·/)).toBeVisible();
  expect(screen.queryByText("I think I like the frame")).toBeNull();
  await userEvent.click(screen.getByRole("button", { name: /local · whisper/ }));
  await userEvent.click(await screen.findByRole("button", { name: "Review notes" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/projects/p1/reviews/rtarget_one/sessions/rsession_1/runs/rtrans_local"));
});

test("transcription preflight blockers use readable copy and hide the start button", async () => {
  renderApp("/projects/p1/reviews/rtarget_one/sessions/rsession_1", {
    getReviewSession: vi.fn().mockResolvedValue({ ...session, state: "completed" }),
    reviewSessionAudioUrl: vi.fn().mockReturnValue("/audio.wav"),
    listReviewTranscriptions: vi.fn().mockResolvedValue({ items: [] }),
    preflightReviewTranscription: vi.fn().mockResolvedValue({ ready: false, blockers: ["review_transcription_local_model_unavailable"], warnings: [], backend: "local", provider: "local", model: "m", audioFingerprint: null, estimate: null, existingRunId: null }),
  });
  await userEvent.click(await screen.findByRole("button", { name: "Check" }));
  expect(await screen.findByText(/Speech models aren't installed/)).toBeVisible();
  expect(screen.queryByRole("button", { name: "Transcribe" })).toBeNull();
});

/* ---------- Editorial workspace ---------- */

test("the review workspace corrects speakers and groups the transcript", async () => {
  const grouped = workspace({
    discussions: [{
      discussionId: "d1", title: "Framing", groupingSource: "heuristic", state: "open", utteranceIds: ["u1"], startRecordingSeconds: 16.5, endRecordingSeconds: 18,
      languages: ["en"], speakers: ["unknown"], candidateOperationIds: ["editop_b"], targetOperationId: null, targetSource: null, unanchored: false, items: [], updatedAt: "t",
    }],
  });
  const { client } = renderApp("/projects/p1/reviews/rtarget_one/sessions/rsession_1/runs/rtrans_local", {
    getReviewWorkspace: vi.fn().mockResolvedValue(workspace()),
    correctReviewUtterance: vi.fn().mockResolvedValue(workspace()),
    proposeReviewDiscussions: vi.fn().mockResolvedValue(grouped),
    updateReviewDiscussion: vi.fn().mockResolvedValue(grouped),
    reviewReportUrl: vi.fn().mockReturnValue("/report.json"),
  });
  expect(await screen.findByRole("heading", { name: /Review notes/ })).toBeVisible();
  expect(screen.getByText("No timing anchors")).toBeVisible();
  await userEvent.click(screen.getByRole("button", { name: "Transcript (1)" }));
  await userEvent.selectOptions(screen.getByLabelText("Speaker for u1"), "A");
  expect(client.correctReviewUtterance).toHaveBeenCalledWith("p1", "rtrans_local", "u1", "speaker", "A");
  await userEvent.click(screen.getByRole("button", { name: /Discussions/ }));
  await userEvent.click(screen.getByRole("button", { name: "Group transcript into discussions" }));
  const card = await screen.findByRole("article", { name: "Discussion Framing" });
  const shot = within(card).getByLabelText("Shot this is about");
  // Time-based candidates come first and are marked, but never pre-selected.
  expect(within(shot).getAllByRole("option")[2]).toHaveTextContent("★ #2");
  expect(shot).toHaveValue("");
  await userEvent.selectOptions(shot, "editop_b");
  expect(client.updateReviewDiscussion).toHaveBeenCalledWith("p1", "d1", { targetOperationId: "editop_b" });
});

test("transcript lines show mapped video time and their shot thumbnail", async () => {
  const mapped = workspace();
  mapped.utterances[0].movieTime = { movieSeconds: 1.5, uncertaintySeconds: 0.25, source: "sharedSync:estimated", evidenceIds: ["player_a", "player_b"] };
  renderApp("/projects/p1/reviews/rtarget_one/sessions/rsession_1", {
    ...targetsClient,
    getReviewSession: vi.fn().mockResolvedValue({ ...session, state: "completed" }),
    reviewSessionAudioUrl: vi.fn().mockReturnValue("/audio.wav"),
    listReviewTranscriptions: vi.fn().mockResolvedValue({ items: [run()] }),
    getReviewWorkspace: vi.fn().mockResolvedValue(mapped),
    getReviewTarget: vi.fn().mockResolvedValue({ ...target, youtube: { ...target.youtube, videoId: "dQw4w9WgXcQ" } }),
  });
  const link = await screen.findByRole("link", { name: "Video ≈ 0:01.5" });
  expect(link).toHaveAttribute("href", "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1s");
  expect(screen.getByText("Audio")).toBeVisible();
  const image = screen.getByRole("img", { name: "Shot 1 thumbnail" });
  expect(image).toHaveAttribute("src", "/api/v1/projects/p1/assets/asset_repeated/thumbnail");
  fireEvent.error(image);
  expect(await screen.findByText("Thumbnail unavailable")).toBeVisible();
  expect(link).toBeVisible();
});

test("an unmapped transcript line does not borrow a nearby video time or thumbnail", async () => {
  renderApp("/projects/p1/reviews/rtarget_one/sessions/rsession_1", {
    ...targetsClient,
    getReviewSession: vi.fn().mockResolvedValue({ ...session, state: "completed" }),
    reviewSessionAudioUrl: vi.fn().mockReturnValue("/audio.wav"),
    listReviewTranscriptions: vi.fn().mockResolvedValue({ items: [run()] }),
    getReviewWorkspace: vi.fn().mockResolvedValue(workspace()),
  });
  expect(await screen.findByText("Video time unavailable")).toBeVisible();
  expect(screen.getByText("I think I like the frame")).toBeVisible();
  expect(screen.queryByRole("img")).toBeNull();
  expect(screen.queryByRole("link", { name: /^Video/ })).toBeNull();
});

test("named cuts appear in the review list with revision identity as secondary text", async () => {
  renderApp("/projects/p1/reviews", { ...targetsClient, listReviewTargets: vi.fn().mockResolvedValue({ items: [{ ...target, displayName: "RC7" }], nextBefore: null }) });
  expect(await screen.findByRole("link", { name: /RC7/ })).toBeVisible();
  expect(screen.getByText(/Revision syntheti/)).toBeVisible();
});

test("renaming a cut updates its heading without replacing the target", async () => {
  const named = { ...target, displayName: "RC7" };
  const { client } = renderApp("/projects/p1/reviews/rtarget_one", { ...targetsClient, renameReviewTarget: vi.fn().mockResolvedValue(named) });
  await userEvent.click(await screen.findByRole("button", { name: "Rename cut" }));
  const dialog = await screen.findByRole("dialog", { name: "Rename cut" });
  await userEvent.type(within(dialog).getByLabelText("Cut name"), "RC7");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save name" }));
  await waitFor(() => expect(client.renameReviewTarget).toHaveBeenCalledWith("p1", "rtarget_one", "RC7"));
  expect(await screen.findByRole("heading", { name: /^RC7/ })).toBeVisible();
});
