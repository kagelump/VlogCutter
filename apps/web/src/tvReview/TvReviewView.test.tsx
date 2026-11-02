import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ProjectClientError, type BrowserProjectClient, type ReviewTargetDetail, type ReviewTargetPreflight } from "@rcdvlog/project-client";
import { TvReviewView, formatSeconds } from "./TvReviewView";

const detail: ReviewTargetDetail = {
  reviewTargetId: "rtarget_one",
  projectId: "project_fixture",
  revisionId: "editrev_synthetic",
  viewedArtifactId: "preview_synthetic",
  targetDigest: "a".repeat(64),
  durationSeconds: 6.02,
  cutCount: 3,
  youtube: { videoId: null, declaredBy: null, identityStatus: "absent" },
  importedAt: "2026-11-02T00:00:00.000Z",
  latestVerifiedAt: "2026-11-02T00:00:00Z",
  readiness: { ready: true, freshness: "fresh", verificationAgeSeconds: 10, maxVerificationAgeSeconds: 604800, availabilityState: "present", blockers: [] },
  schemaVersion: 1,
  planDigest: "b".repeat(64),
  constraintsKey: "c".repeat(64),
  movie: { blake3: "d".repeat(64), byteLength: 123456, durationSeconds: 6.02, timeBaseNumerator: 1, timeBaseDenominator: 90000, originPts: 90000, videoFrameCount: 180, audioSampleRate: 48000, audioChannels: 2 },
  mapping: { status: "measuredComplete", picturePrecision: "decodedOutputPts", sourceRangePrecision: "declaredSourceClock", audioDeclaration: "declaredNotMeasured", cutCount: 3, pictureEndSeconds: 6 },
  cuts: [
    { operationId: "editop_split_a", position: 0, assetId: "asset_repeated", sourceInSeconds: 1, sourceOutSeconds: 3, movieInPts: 90000, movieOutPts: 270000, outputFrameCount: 60, declaredAudio: null, movieInSeconds: 0, movieOutSeconds: 2 },
    { operationId: "editop_split_b", position: 1, assetId: "asset_repeated", sourceInSeconds: 3, sourceOutSeconds: 5, movieInPts: 270000, movieOutPts: 450000, outputFrameCount: 60, declaredAudio: { mode: "silent" }, movieInSeconds: 2, movieOutSeconds: 4 },
  ],
  provenance: { importedAt: "2026-11-02T00:00:00.000Z", importer: { actorType: "browser_session", actorId: "session_x" }, declaredIssuer: "Travel Mac", issuerVerification: "unverified", derivedFromReviewTargetId: null },
  observations: [],
  limitations: ["A YouTube video ID is a declaration only."],
};

function client(overrides: Partial<BrowserProjectClient> = {}): BrowserProjectClient {
  return {
    listProjects: vi.fn().mockResolvedValue({ items: [{ projectId: "project_fixture", state: "registered", assetCount: 1 }], nextCursor: null }),
    listReviewTargets: vi.fn().mockResolvedValue({ items: [], nextBefore: null }),
    preflightReviewTarget: vi.fn(),
    importReviewTarget: vi.fn(),
    getReviewTarget: vi.fn().mockResolvedValue(detail),
    declareReviewTargetYoutube: vi.fn(),
    getReviewTargetCandidates: vi.fn(),
    ...overrides,
  } as unknown as BrowserProjectClient;
}

const handoff = { schemaVersion: 1, targetDigest: "x", target: {}, verification: {} };

test("preflight shows checks and only offers import when every check passes", async () => {
  const preflight: ReviewTargetPreflight = {
    outcome: "new", importable: true, existingReviewTargetId: null, conflictingReviewTargetIds: [], maxVerificationAgeSeconds: 604800,
    preview: { projectId: "project_fixture", revisionId: "editrev_synthetic", viewedArtifactId: "preview_synthetic", targetDigest: "a".repeat(64), movieBlake3: "d".repeat(64), durationSeconds: 6.02, cutCount: 3, youtubeVideoId: "synthetic01", verifiedAt: "2026-11-02T00:00:00Z" },
    checks: [{ name: "targetDigest", status: "pass", code: null, detail: "recomputed by Rust" }, { name: "youtube", status: "warn", code: null, detail: "declared, not verified" }],
  };
  const navigate = vi.fn();
  const mock = client({
    preflightReviewTarget: vi.fn().mockResolvedValue(preflight),
    importReviewTarget: vi.fn().mockResolvedValue({ outcome: "created", reviewTarget: detail }),
  });
  render(<TvReviewView client={mock} path="/tv-review/project_fixture" navigate={navigate} />);
  expect(await screen.findByText("No review targets yet")).toBeVisible();
  // The CLI envelope is accepted; only its data member is sent.
  const envelope = JSON.stringify({ ok: true, command: "edit-plan.review-target-export", data: handoff });
  await userEvent.click(screen.getByLabelText("Handoff JSON"));
  await userEvent.paste(envelope);
  await userEvent.type(screen.getByLabelText("Owning host label (optional)"), "Travel Mac");
  await userEvent.click(screen.getByRole("button", { name: "Check handoff" }));
  expect(await screen.findByText("Ready to import as a new immutable target.")).toBeVisible();
  expect(mock.preflightReviewTarget).toHaveBeenCalledWith("project_fixture", { handoff, declaredIssuer: "Travel Mac" });
  expect(screen.getByText("synthetic01 (declared, unverified)")).toBeVisible();
  await userEvent.click(screen.getByRole("button", { name: "Import target" }));
  await waitFor(() => expect(navigate).toHaveBeenCalledWith("/tv-review/project_fixture/rtarget_one"));
});

test("a blocked preflight explains the stable code and offers no import", async () => {
  const mock = client({
    preflightReviewTarget: vi.fn().mockResolvedValue({
      outcome: "blocked", importable: false, preview: null, existingReviewTargetId: null, conflictingReviewTargetIds: [], maxVerificationAgeSeconds: 604800,
      checks: [{ name: "targetDigest", status: "fail", code: "review_target_digest_mismatch", detail: "review target digest mismatch" }],
    }),
  });
  render(<TvReviewView client={mock} path="/tv-review/project_fixture" navigate={vi.fn()} />);
  await userEvent.click(await screen.findByLabelText("Handoff JSON"));
  await userEvent.paste(JSON.stringify(handoff));
  await userEvent.click(screen.getByRole("button", { name: "Check handoff" }));
  expect(await screen.findByText(/modified after export/)).toBeVisible();
  expect(screen.queryByRole("button", { name: "Import target" })).toBeNull();
});

test("malformed JSON never reaches the server", async () => {
  const mock = client();
  render(<TvReviewView client={mock} path="/tv-review/project_fixture" navigate={vi.fn()} />);
  await userEvent.click(await screen.findByLabelText("Handoff JSON"));
  await userEvent.paste("{not json");
  await userEvent.click(screen.getByRole("button", { name: "Check handoff" }));
  expect(await screen.findByText(/not valid JSON/)).toBeVisible();
  expect(mock.preflightReviewTarget).not.toHaveBeenCalled();
});

test("an import conflict is shown by code and not retried", async () => {
  const mock = client({
    preflightReviewTarget: vi.fn().mockResolvedValue({ outcome: "new", importable: true, preview: null, existingReviewTargetId: null, conflictingReviewTargetIds: [], maxVerificationAgeSeconds: 1, checks: [] }),
    importReviewTarget: vi.fn().mockRejectedValue(new ProjectClientError("server_error", "The server could not complete the request.", "review_target_conflict", "req_1", 409)),
  });
  render(<TvReviewView client={mock} path="/tv-review/project_fixture" navigate={vi.fn()} />);
  await userEvent.click(await screen.findByLabelText("Handoff JSON"));
  await userEvent.paste(JSON.stringify(handoff));
  await userEvent.click(screen.getByRole("button", { name: "Check handoff" }));
  await userEvent.click(await screen.findByRole("button", { name: "Import target" }));
  expect(await screen.findByText(/Nothing was overwritten/)).toBeVisible();
  expect(mock.importReviewTarget).toHaveBeenCalledTimes(1);
});

test("detail shows revision, digest, duration and mapping, and declares a parsed YouTube ID", async () => {
  const navigate = vi.fn();
  const mock = client({
    declareReviewTargetYoutube: vi.fn().mockResolvedValue({ outcome: "created", reviewTarget: { ...detail, reviewTargetId: "rtarget_two" } }),
  });
  render(<TvReviewView client={mock} path="/tv-review/project_fixture/rtarget_one" navigate={navigate} />);
  expect(await screen.findByRole("heading", { name: "Revision editrev_synthetic" })).toBeVisible();
  expect(screen.getByText("Verified measured mapping", { exact: false })).toBeVisible();
  expect(screen.getByText("dddddddddddd…")).toBeVisible();
  expect(screen.getByText(/0:06.02/)).toBeVisible();
  expect(screen.getByText("Ready for a session")).toBeVisible();
  await userEvent.type(screen.getByLabelText("YouTube link or video ID"), "https://example.com/watch?v=dQw4w9WgXcQ");
  await userEvent.click(screen.getByRole("button", { name: "Declare video" }));
  expect(await screen.findByText(/Paste a YouTube watch/)).toBeVisible();
  expect(mock.declareReviewTargetYoutube).not.toHaveBeenCalled();
  await userEvent.clear(screen.getByLabelText("YouTube link or video ID"));
  await userEvent.type(screen.getByLabelText("YouTube link or video ID"), "https://youtu.be/dQw4w9WgXcQ");
  await userEvent.click(screen.getByRole("button", { name: "Declare video" }));
  await waitFor(() => expect(mock.declareReviewTargetYoutube).toHaveBeenCalledWith("project_fixture", "rtarget_one", "dQw4w9WgXcQ"));
  expect(navigate).toHaveBeenCalledWith("/tv-review/project_fixture/rtarget_two");
});

test("a stale target blocks new sessions while keeping the snapshot visible", async () => {
  const stale = { ...detail, readiness: { ...detail.readiness, ready: false, freshness: "expired" as const, blockers: ["review_target_verification_expired"] } };
  render(<TvReviewView client={client({ getReviewTarget: vi.fn().mockResolvedValue(stale) })} path="/tv-review/project_fixture/rtarget_one" navigate={vi.fn()} />);
  expect(await screen.findByText(/New recording sessions are blocked/)).toBeVisible();
  expect(screen.getByText("Verification expired")).toBeVisible();
  expect(screen.getByRole("heading", { name: "Revision editrev_synthetic" })).toBeVisible();
});

test("formats elapsed movie seconds", () => {
  expect(formatSeconds(0)).toBe("0:00.00");
  expect(formatSeconds(62.5)).toBe("1:02.50");
});
