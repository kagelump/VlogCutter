import type { ReviewSessionDetail, ReviewTargetDetail, ReviewTranscriptionRun, ReviewWorkspace } from "@rcdvlog/project-client";

export const target: ReviewTargetDetail = {
  reviewTargetId: "rtarget_one",
  contract: "reviewTargetV1",
  projectId: "p1",
  revisionId: "editrev_synthetic",
  viewedArtifactId: "preview_synthetic",
  targetDigest: "a".repeat(64),
  durationSeconds: 6.02,
  cutCount: 2,
  youtube: { videoId: null, declaredBy: null, identityStatus: "absent" },
  importedAt: "2026-11-02T00:00:00.000Z",
  latestVerifiedAt: "2026-11-02T00:00:00Z",
  readiness: { ready: true, freshness: "fresh", verificationAgeSeconds: 10, maxVerificationAgeSeconds: 604800, availabilityState: "present", blockers: [] },
  schemaVersion: 1,
  planDigest: "b".repeat(64),
  constraintsKey: "c".repeat(64),
  movie: { blake3: "d".repeat(64), byteLength: 123456, durationSeconds: 6.02, timeBaseNumerator: 1, timeBaseDenominator: 90000, originPts: 90000, videoFrameCount: 180, audioSampleRate: 48000, audioChannels: 2 },
  mapping: { status: "measuredComplete", picturePrecision: "decodedOutputPts", sourceRangePrecision: "declaredSourceClock", audioDeclaration: "declaredNotMeasured", cutCount: 2, pictureEndSeconds: 6 },
  cuts: [
    { operationId: "editop_a", position: 0, assetId: "asset_repeated", sourceInSeconds: 1, sourceOutSeconds: 3, movieInPts: 90000, movieOutPts: 270000, outputFrameCount: 60, declaredAudio: null, movieInSeconds: 0, movieOutSeconds: 2 },
    { operationId: "editop_b", position: 1, assetId: "asset_repeated", sourceInSeconds: 3, sourceOutSeconds: 5, movieInPts: 270000, movieOutPts: 450000, outputFrameCount: 60, declaredAudio: { mode: "silent" }, movieInSeconds: 2, movieOutSeconds: 4 },
  ],
  provenance: { importedAt: "2026-11-02T00:00:00.000Z", importer: { actorType: "browser_session", actorId: "session_x" }, declaredIssuer: "Travel Mac", issuerVerification: "unverified", derivedFromReviewTargetId: null },
  observations: [],
  limitations: ["A YouTube video ID is a declaration only."],
};

export const session = {
  sessionId: "rsession_1", projectId: "p1", reviewTargetId: "rtarget_one", targetDigest: "a".repeat(64), revisionId: "editrev_synthetic", viewedArtifactId: "x",
  state: "incomplete", abandoned: false, endReason: "deviceLost", sampleRate: 48000, channels: 1, encoding: "pcm_s16le", createdAt: "2026-11-02T14:04:39Z", finalizedAt: "2026-11-02T14:08:00Z", deletedAt: null,
  receivedSeconds: 4, manualCueCount: 1,
  consent: { allPresentAcknowledged: true, additionalSpeakersDisclosed: false },
  capture: { echoCancellation: false, noiseSuppression: false, autoGainControl: false, clock: "audioWorkletCurrentFrame", baseLatencySeconds: null, userAgentFamily: "chromium" },
  createdBy: { actorType: "browser_session", actorId: "s" }, clientStartedPerformanceMs: null, recordingExpiresAt: "t",
  coverage: { receivedFrames: 192000, receivedBytes: 384000, chunkCount: 2, highestSequence: 2, missingSequences: [1], gaps: [{ startFrame: 96000, endFrame: 192000 }], finalFrameCount: 288000, receivedSeconds: 4 },
  chunks: [],
  events: [{ eventId: "c", kind: "manualSyncCue", audioFrame: 48000, audioSeconds: 1, performanceMs: 1, declaredMovieSeconds: 0, serverReceivedAt: "t", detail: null }],
  limits: { maxChunkBytes: 1, maxSessionBytes: 1, recordingLifetimeHours: 12 },
  storage: { location: "Private server data directory.", retention: "Kept until you delete it.", backup: "Not in backups." },
} as unknown as ReviewSessionDetail;

export function run(overrides: Partial<ReviewTranscriptionRun> = {}): ReviewTranscriptionRun {
  return {
    schema: "review-transcript.v1", runId: "rtrans_local", projectId: "p1", sessionId: "rsession_1", reviewTargetId: "rtarget_one", revisionId: "editrev_synthetic",
    backend: "local", provider: "local", model: "whisper", recipe: "r", settings: { backend: "local" }, audioFingerprint: "f", sampleRate: 48000,
    state: "completed", errorCode: null, attemptCount: 1, jobId: null, createdAt: "2026-11-02T14:11:57Z", updatedAt: "2026-11-02T14:20:00Z",
    createdBy: { actorType: "browser_session", actorId: "s" }, segments: [], recordingGaps: [],
    utterances: [{
      utteranceId: "u1", segmentIndex: 0, windowIndex: 0, startFrame: 0, endFrame: 48000, startSeconds: 16.5, endSeconds: 18, text: "I think I like the frame",
      language: "en", script: "Latn", displayText: "I think I like the frame", displayProjection: null, speaker: "unknown", speakerEvidence: "none",
      meanTokenProbability: 0.9, flags: [], suppressed: false,
    }],
    spend: { authorizedMaxUsd: null, reservedUsd: 0, knownCostUsd: 0, unknownCostWindows: 0, dispatchedWindows: 0, cachedWindows: 0 },
    diarization: { status: "unavailable", reason: "Speakers are not identified." },
    limitations: [],
    ...overrides,
  };
}

export function workspace(overrides: Partial<ReviewWorkspace> = {}): ReviewWorkspace {
  const utterance = run().utterances[0];
  return {
    runId: "rtrans_local", sessionId: "rsession_1", projectId: "p1", reviewTargetId: "rtarget_one", revisionId: "editrev_synthetic", viewedArtifactId: "x",
    targetDigest: "a".repeat(64), targetContract: "reviewTargetV1",
    sync: { status: "noAnchors", source: "s", note: "No anchors.", calibratedClocks: 0, anchors: 0, boundaries: 0, policy: { minimumConfidence: 0, maxAnchorGapSeconds: 0, maxExtrapolationSeconds: 0, maxDriftPpm: 0, residualToleranceSeconds: 0, unobservedUncertaintyPpm: 0 }, diagnostics: [] },
    cuts: target.cuts.map(({ operationId, position, assetId, movieInSeconds, movieOutSeconds }) => ({ operationId, position, assetId, movieInSeconds, movieOutSeconds })),
    utterances: [{
      machine: utterance, text: utterance.text, displayText: utterance.displayText, speaker: "unknown", language: "en", excluded: false,
      movieTime: null, sync: { state: "unknown", slices: [], uncoveredFrames: 0 }, candidateOperationIds: [], referenceHint: null, corrections: [],
    }],
    discussions: [],
    coverage: { audiovisualReviewAttested: false, note: "Audio only." },
    ...overrides,
  };
}
