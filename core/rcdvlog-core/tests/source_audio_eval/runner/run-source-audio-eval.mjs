#!/usr/bin/env node
// Reusable source-audio evaluation runner.
//
// Drives a built `rcdvlog` binary over a manifest:
//   1. index the movie  (tv-review source-audio-landmark-index)
//   2. match each recording (tv-review source-audio-landmark-match)
//   3. collect each match's report JSON and the run receipt JSON
//
// Explicit process contract (all four required):
//   --binary <path>     built rcdvlog binary
//   --manifest <path>   evaluation manifest JSON (see README "Manifest shape")
//   --media-root <dir>  directory under which the manifest's media paths resolve
//   --output <path>     run receipt/output JSON path
// Options:
//   --work-dir <path>   scratch directory for index/request/report JSON
//                       (default: "<output>.work"; must not already exist)
//   --dry-run           print and validate the plan without executing anything;
//                       runnable before the imported CLI lands (the binary does
//                       not need to exist in dry-run mode)
//
// Exit codes: 0 success / valid plan, 1 validation or execution failure,
// 2 usage error.

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, join, resolve, sep } from "node:path";
import { parseArgs } from "node:util";

const OFFSET_CONVENTION =
  "movieElapsedSeconds = recordingSecondsWithinEpoch + offsetSeconds";
const STEP_TIMEOUT_MS = 900_000;
const STEP_MAX_BUFFER = 64 * 1024 * 1024;

// The preserved CLI request schema for the match step
// (LandmarkMatchInput; pooledOffsetWindows / pooledOffsetModeLimit included).
const REQUEST_KEYS = new Set([
  "captureClockId",
  "policy",
  "searchPolicy",
  "diagnosticOffsetEvidencePerTile",
  "pooledOffsetWindows",
  "pooledOffsetModeLimit",
  "sync",
]);
const POLICY_KEYS = [
  "minimumTileAnchors",
  "minimumTileSeconds",
  "minimumFrequencyDiversity",
  "minimumSegmentTiles",
  "minimumSegmentAnchors",
  "minimumSegmentSeconds",
  "scoreBaselinePerTile",
  "segmentChangePenalty",
  "ambiguityMargin",
  "maxDriftPpm",
  "maximumGapSeconds",
  "gapPenaltyPerSecond",
  "anchorStepSeconds",
];
const SYNC_POLICY_KEYS = [
  "minimumConfidence",
  "maxAnchorGapSeconds",
  "maxExtrapolationSeconds",
  "maxDriftPpm",
  "residualToleranceSeconds",
  "unobservedUncertaintyPpm",
];
const CLOCK_KEYS = [
  "clockId",
  "sampleRate",
  "firstSample",
  "lastSample",
  "monotonicOriginNs",
  "monotonicToleranceNs",
];
const MOVIE_CLOCK_KEYS = [
  "movieDigest",
  "timeBaseNumerator",
  "timeBaseDenominator",
  "originPts",
  "durationPts",
];

function usage() {
  console.error(
    "Usage: run-source-audio-eval.mjs --binary <path> --manifest <path> --media-root <dir> --output <path> [--work-dir <path>] [--dry-run]",
  );
  process.exit(2);
}

const { values } = parseArgs({
  options: {
    binary: { type: "string" },
    manifest: { type: "string" },
    "media-root": { type: "string" },
    output: { type: "string" },
    "work-dir": { type: "string" },
    "dry-run": { type: "boolean" },
  },
  allowPositionals: false,
});
for (const key of ["binary", "manifest", "media-root", "output"]) {
  if (!values[key]) usage();
}
const dryRun = values["dry-run"] === true;

function fail(message) {
  console.error(`invalid plan: ${message}`);
  process.exit(1);
}

function sha256Bytes(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function sha256File(path) {
  return sha256Bytes(readFileSync(path));
}

function isInt(value) {
  return Number.isInteger(value);
}

function isFiniteNumber(value) {
  return typeof value === "number" && Number.isFinite(value);
}

function isOpaqueId(value) {
  return (
    typeof value === "string" &&
    value.length >= 1 &&
    value.length <= 255 &&
    /^[A-Za-z0-9_-]+$/.test(value)
  );
}

function isDigest(value) {
  return typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
}

function expectedOffsetForWindow(expected, j) {
  return expected.windows && expected.windows[j]
    ? expected.windows[j].expectedOffsetSeconds
    : expected.offsetSeconds;
}

function canonicalJson(value) {
  if (Array.isArray(value)) {
    return "[" + value.map(canonicalJson).join(",") + "]";
  }
  if (value !== null && typeof value === "object") {
    return (
      "{" +
      Object.keys(value)
        .sort()
        .map((k) => JSON.stringify(k) + ":" + canonicalJson(value[k]))
        .join(",") +
      "}"
    );
  }
  return JSON.stringify(value);
}

function resolveMediaPath(mediaRoot, declared) {
  if (typeof declared !== "string" || declared.length === 0) {
    fail(`media path must be a non-empty string: ${JSON.stringify(declared)}`);
  }
  if (isAbsolute(declared) || declared.split(/[\\/]/).includes("..")) {
    fail(`media path must be relative and stay under --media-root: ${declared}`);
  }
  const resolved = resolve(mediaRoot, declared);
  if (!resolved.startsWith(resolve(mediaRoot) + sep)) {
    fail(`media path escapes --media-root: ${declared}`);
  }
  return resolved;
}

function validateMovieClock(where, clock, reference) {
  if (clock === null || typeof clock !== "object" || Array.isArray(clock)) {
    fail(`${where} must be a movie clock object`);
  }
  for (const key of MOVIE_CLOCK_KEYS) {
    if (!(key in clock)) fail(`${where} is missing ${key}`);
  }
  if (!isDigest(clock.movieDigest)) fail(`${where}.movieDigest must be 64 lowercase hex`);
  if (!isInt(clock.timeBaseNumerator) || clock.timeBaseNumerator <= 0) {
    fail(`${where}.timeBaseNumerator must be a positive integer`);
  }
  if (!isInt(clock.timeBaseDenominator) || clock.timeBaseDenominator <= 0) {
    fail(`${where}.timeBaseDenominator must be a positive integer`);
  }
  if (!isInt(clock.originPts)) fail(`${where}.originPts must be an integer`);
  if (!isInt(clock.durationPts) || clock.durationPts <= 0) {
    fail(`${where}.durationPts must be a positive integer`);
  }
  if (reference !== undefined && canonicalJson(clock) !== canonicalJson(reference)) {
    fail(`${where} must equal the manifest movie clock exactly`);
  }
}

// ---------------------------------------------------------------------------
// Manifest validation (shared by --dry-run and execute mode).

const mediaRoot = resolve(values["media-root"]);
if (!existsSync(mediaRoot) || !statSync(mediaRoot).isDirectory()) {
  fail(`--media-root is not a directory: ${mediaRoot}`);
}
const manifestPath = resolve(values.manifest);
if (!existsSync(manifestPath)) fail(`--manifest not found: ${manifestPath}`);
let manifest;
try {
  manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
} catch (error) {
  fail(`--manifest is not valid JSON: ${error.message}`);
}
if (manifest.schemaVersion !== 1) fail("manifest.schemaVersion must be 1");
if (manifest.offsetConvention !== OFFSET_CONVENTION) {
  fail(`manifest.offsetConvention must be exactly: ${OFFSET_CONVENTION}`);
}
if (typeof manifest.fixtureSet !== "string" || manifest.fixtureSet.length === 0) {
  fail("manifest.fixtureSet must be a non-empty string");
}

// Movie and index request.
const movie = manifest.movie;
if (movie === null || typeof movie !== "object" || Array.isArray(movie)) {
  fail("manifest.movie must be an object");
}
validateMovieClock("manifest.movie.clock", movie.clock);
const moviePath = resolveMediaPath(mediaRoot, movie.path);
const indexRequest = manifest.indexRequest;
if (indexRequest === null || typeof indexRequest !== "object" || Array.isArray(indexRequest)) {
  fail("manifest.indexRequest must be an object");
}
validateMovieClock("manifest.indexRequest.movie", indexRequest.movie, movie.clock);
if (![512, 1024].includes(indexRequest.fftSize)) {
  fail("manifest.indexRequest.fftSize must be 512 or 1024");
}
if (!isInt(indexRequest.hashesPerSecond) || indexRequest.hashesPerSecond < 20 || indexRequest.hashesPerSecond > 100) {
  fail("manifest.indexRequest.hashesPerSecond must be an integer in 20..=100");
}
if (!isInt(indexRequest.fanout) || indexRequest.fanout < 1 || indexRequest.fanout > 3) {
  fail("manifest.indexRequest.fanout must be an integer in 1..=3");
}

const media = [];
function checkMedia(where, entry) {
  if (entry === null || typeof entry !== "object" || Array.isArray(entry)) {
    fail(`${where} must be a media object`);
  }
  const resolved = resolveMediaPath(mediaRoot, entry.path);
  if (!existsSync(resolved) || !statSync(resolved).isFile()) {
    fail(`${where} media file not found under --media-root: ${entry.path}`);
  }
  const sha256 = sha256File(resolved);
  if (entry.sha256 !== undefined && entry.sha256 !== sha256) {
    fail(`${where} sha256 mismatch for ${entry.path}: manifest ${entry.sha256}, actual ${sha256}`);
  }
  if (!isInt(entry.sampleRate) || entry.sampleRate <= 0) {
    fail(`${where}.sampleRate must be a positive integer`);
  }
  media.push({ path: entry.path, sha256, bytes: statSync(resolved).size, declaredSha256: entry.sha256 ?? null });
  return resolved;
}
checkMedia("manifest.movie", movie);

// Cases.
const cases = manifest.cases;
if (!Array.isArray(cases) || cases.length === 0) fail("manifest.cases must be a non-empty array");
const seenCaseIds = new Set();
const seenWindowKeys = new Set();
const validatedCases = cases.map((entry, i) => {
  const where = `manifest.cases[${i}]`;
  if (entry === null || typeof entry !== "object" || Array.isArray(entry)) {
    fail(`${where} must be an object`);
  }
  if (!isOpaqueId(entry.caseId)) fail(`${where}.caseId must match [A-Za-z0-9_-]{1,255}`);
  if (seenCaseIds.has(entry.caseId)) fail(`duplicate caseId: ${entry.caseId}`);
  seenCaseIds.add(entry.caseId);

  const recording = entry.recording;
  const recordingPath = checkMedia(`${where}.recording`, recording);
  if (!isInt(recording.firstSample) || !isInt(recording.lastSample) || recording.firstSample >= recording.lastSample) {
    fail(`${where}.recording firstSample/lastSample must be a half-open integer range`);
  }
  const durationSeconds = (recording.lastSample - recording.firstSample) / recording.sampleRate;

  const request = entry.request;
  if (request === null || typeof request !== "object" || Array.isArray(request)) {
    fail(`${where}.request must be a LandmarkMatchInput object`);
  }
  for (const key of Object.keys(request)) {
    if (!REQUEST_KEYS.has(key)) fail(`${where}.request has unknown key: ${key}`);
  }
  for (const key of REQUEST_KEYS) {
    if (key !== "policy" && key !== "searchPolicy" && key !== "diagnosticOffsetEvidencePerTile"
        && key !== "pooledOffsetWindows" && key !== "pooledOffsetModeLimit" && !(key in request)) {
      fail(`${where}.request is missing required key: ${key}`);
    }
  }
  if (!isOpaqueId(request.captureClockId)) fail(`${where}.request.captureClockId must match [A-Za-z0-9_-]{1,255}`);
  const sync = request.sync;
  if (sync === null || typeof sync !== "object" || Array.isArray(sync)) {
    fail(`${where}.request.sync must be a SyncRequest object`);
  }
  if (sync.schemaVersion !== 1) fail(`${where}.request.sync.schemaVersion must be 1`);
  if (!isOpaqueId(sync.sessionId)) fail(`${where}.request.sync.sessionId must match [A-Za-z0-9_-]{1,255}`);
  if (!isDigest(sync.targetDigest)) fail(`${where}.request.sync.targetDigest must be 64 lowercase hex`);
  validateMovieClock(`${where}.request.sync.movie`, sync.movie, movie.clock);
  if (!Array.isArray(sync.captureClocks) || sync.captureClocks.length === 0) {
    fail(`${where}.request.sync.captureClocks must be a non-empty array`);
  }
  for (const [j, clock] of sync.captureClocks.entries()) {
    for (const key of CLOCK_KEYS) {
      if (!(key in clock)) fail(`${where}.request.sync.captureClocks[${j}] is missing ${key}`);
    }
  }
  const clock = sync.captureClocks.find((c) => c.clockId === request.captureClockId);
  if (clock === undefined) fail(`${where}.request.captureClockId is absent from sync.captureClocks`);
  if (clock.sampleRate !== recording.sampleRate
      || clock.firstSample !== recording.firstSample
      || clock.lastSample !== recording.lastSample) {
    fail(`${where}: capture clock ${clock.clockId} must match the declared recording sample range`);
  }
  if (!Array.isArray(sync.anchors) || !Array.isArray(sync.boundaries)) {
    fail(`${where}.request.sync.anchors/boundaries must be arrays`);
  }
  if (sync.policy === undefined) fail(`${where}.request.sync.policy is required`);
  for (const key of SYNC_POLICY_KEYS) {
    if (!isFiniteNumber(sync.policy[key])) fail(`${where}.request.sync.policy.${key} must be a finite number`);
  }
  if (request.policy !== undefined) {
    for (const key of POLICY_KEYS) {
      if (!isFiniteNumber(request.policy[key])) fail(`${where}.request.policy.${key} must be a finite number`);
    }
  }
  if (request.diagnosticOffsetEvidencePerTile !== undefined
      && (!isInt(request.diagnosticOffsetEvidencePerTile)
          || request.diagnosticOffsetEvidencePerTile < 1
          || request.diagnosticOffsetEvidencePerTile > 512)) {
    fail(`${where}.request.diagnosticOffsetEvidencePerTile must be an integer in 1..=512`);
  }

  const windows = request.pooledOffsetWindows;
  if (!Array.isArray(windows) || windows.length === 0 || windows.length > 256) {
    fail(`${where}.request.pooledOffsetWindows must contain 1..256 windows`);
  }
  for (const [j, w] of windows.entries()) {
    if (!isInt(w.startSeconds) || w.startSeconds < 0) {
      fail(`${where}.request.pooledOffsetWindows[${j}].startSeconds must be a non-negative integer`);
    }
    if (![10, 20, 30].includes(w.durationSeconds)) {
      fail(`${where}.request.pooledOffsetWindows[${j}].durationSeconds must be 10, 20 or 30 seconds`);
    }
    if (w.startSeconds + w.durationSeconds > durationSeconds) {
      fail(`${where}.request.pooledOffsetWindows[${j}] exceeds the declared recording duration`);
    }
    const key = `${w.startSeconds}/${w.durationSeconds}`;
    if (seenWindowKeys.has(`${entry.caseId}:${key}`)) {
      fail(`${where}.request.pooledOffsetWindows contains a duplicate window ${key}`);
    }
    seenWindowKeys.add(`${entry.caseId}:${key}`);
  }
  if (request.pooledOffsetModeLimit !== undefined
      && (!isInt(request.pooledOffsetModeLimit) || request.pooledOffsetModeLimit < 1 || request.pooledOffsetModeLimit > 64)) {
    fail(`${where}.request.pooledOffsetModeLimit must be an integer in 1..=64`);
  }

  const expected = entry.expected;
  if (expected === null || typeof expected !== "object" || Array.isArray(expected)) {
    fail(`${where}.expected must be an object`);
  }
  if (!isFiniteNumber(expected.offsetSeconds)) fail(`${where}.expected.offsetSeconds must be a finite number`);
  if (!isFiniteNumber(expected.offsetToleranceSeconds) || expected.offsetToleranceSeconds < 0) {
    fail(`${where}.expected.offsetToleranceSeconds must be a non-negative number`);
  }
  const expectedWindows = expected.windows ?? [];
  if (expectedWindows.length !== windows.length) {
    fail(`${where}.expected.windows must mirror request.pooledOffsetWindows`);
  }
  for (const [j, w] of expectedWindows.entries()) {
    if (w.startSeconds !== windows[j].startSeconds || w.durationSeconds !== windows[j].durationSeconds) {
      fail(`${where}.expected.windows[${j}] must mirror request.pooledOffsetWindows[${j}]`);
    }
    if (!isFiniteNumber(w.expectedOffsetSeconds)) {
      fail(`${where}.expected.windows[${j}].expectedOffsetSeconds must be a finite number`);
    }
  }

  return { entry, where, recordingPath, windows, expected };
});

// ---------------------------------------------------------------------------
// Plan construction.

const outputPath = resolve(values.output);
const workDir = resolve(values["work-dir"] ?? outputPath + ".work");
const binaryPath = resolve(values.binary);
const indexInputPath = join(workDir, "index-input.json");
const indexPath = join(workDir, "index.json");
const indexReportPath = join(workDir, "index-report.json");

const indexStep = {
  argv: [
    binaryPath,
    "tv-review",
    "source-audio-landmark-index",
    "--input",
    indexInputPath,
    "--movie",
    moviePath,
    "--output",
    indexPath,
  ],
  indexInputPath,
  indexPath,
  indexReportPath,
};

const matchSteps = validatedCases.map(({ entry, recordingPath }) => ({
  caseId: entry.caseId,
  argv: [
    binaryPath,
    "tv-review",
    "source-audio-landmark-match",
    "--index",
    indexPath,
    "--input",
    join(workDir, `${entry.caseId}.request.json`),
    "--recording",
    recordingPath,
  ],
  requestPath: join(workDir, `${entry.caseId}.request.json`),
  recordingPath,
  reportPath: join(workDir, `${entry.caseId}.report.json`),
}));

const binaryPresent = existsSync(binaryPath) && statSync(binaryPath).isFile();

// ---------------------------------------------------------------------------
// Dry-run: report the validated plan without executing anything.

if (dryRun) {
  const receipt = {
    schemaVersion: 1,
    runner: "run-source-audio-eval.mjs",
    mode: "dry-run",
    status: "plan valid",
    contract: { binaryPath, manifestPath, mediaRoot, outputPath, workDir },
    binaryPresent,
    manifestSha256: sha256File(manifestPath),
    offsetConvention: OFFSET_CONVENTION,
    media,
    indexStep: { argv: indexStep.argv, status: "planned", indexPath },
    cases: matchSteps.map((step, i) => ({
      caseId: step.caseId,
      status: "planned",
      argv: step.argv,
      expectedOffsetSeconds: validatedCases[i].expected.offsetSeconds,
      expectedOffsetToleranceSeconds: validatedCases[i].expected.offsetToleranceSeconds,
      windows: validatedCases[i].windows.map((w, j) => ({
        startSeconds: w.startSeconds,
        durationSeconds: w.durationSeconds,
        expectedOffsetSeconds: expectedOffsetForWindow(validatedCases[i].expected, j),
      })),
    })),
  };
  mkdirSync(dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, JSON.stringify(receipt, null, 2) + "\n");
  console.log(`dry-run plan valid: ${validatedCases.length} case(s), ${matchSteps.length} match step(s)`);
  console.log(`index: ${indexStep.argv.join(" ")}`);
  for (const step of matchSteps) console.log(`match: ${step.argv.join(" ")}`);
  console.log(`plan written to ${outputPath}`);
  process.exit(0);
}

// ---------------------------------------------------------------------------
// Execute mode.

if (!binaryPresent) fail(`--binary not found: ${binaryPath}`);
if (existsSync(workDir)) {
  fail(`--work-dir already exists (pass a fresh directory): ${workDir}`);
}
mkdirSync(workDir, { recursive: true });

let failed = false;

function runStep(step) {
  const result = spawnSync(step.argv[0], step.argv.slice(1), {
    encoding: "utf8",
    maxBuffer: STEP_MAX_BUFFER,
    timeout: STEP_TIMEOUT_MS,
  });
  if (result.error) return { status: "failed", error: result.error.message };
  if (result.status !== 0) {
    return {
      status: "failed",
      error: `exit ${result.status}: ${(result.stderr ?? "").slice(-2000)}`,
    };
  }
  let envelope;
  try {
    envelope = JSON.parse(result.stdout);
  } catch (error) {
    return { status: "failed", error: `stdout is not JSON: ${error.message}` };
  }
  if (envelope.ok !== true) {
    return { status: "failed", error: JSON.stringify(envelope.error ?? envelope).slice(0, 2000) };
  }
  return { status: "ok", envelope };
}

writeFileSync(indexInputPath, JSON.stringify(indexRequest, null, 2) + "\n");
for (const v of validatedCases) {
  writeFileSync(join(workDir, `${v.entry.caseId}.request.json`), JSON.stringify(v.entry.request, null, 2) + "\n");
}

console.log(`index: ${indexStep.argv.join(" ")}`);
const indexResult = runStep(indexStep);
const indexReceipt = {
  argv: indexStep.argv,
  status: indexResult.status,
  reportPath: indexReportPath,
  reportSha256: null,
  indexDigest: null,
  error: indexResult.error ?? null,
};
if (indexResult.status === "ok") {
  writeFileSync(indexReportPath, JSON.stringify(indexResult.envelope, null, 2) + "\n");
  indexReceipt.reportSha256 = sha256File(indexReportPath);
  indexReceipt.indexDigest = indexResult.envelope.data?.indexDigest ?? null;
} else {
  failed = true;
  console.error(`index step failed: ${indexResult.error}`);
}

const caseReceipts = [];
for (const [i, step] of matchSteps.entries()) {
  const { expected, windows } = validatedCases[i];
  const receipt = {
    caseId: step.caseId,
    status: "planned",
    command: step.argv,
    requestPath: step.requestPath,
    reportPath: step.reportPath,
    reportSha256: null,
    expectedOffsetSeconds: expected.offsetSeconds,
    expectedOffsetToleranceSeconds: expected.offsetToleranceSeconds,
    completeness: null,
    windows: windows.map((w, j) => ({
      startSeconds: w.startSeconds,
      durationSeconds: w.durationSeconds,
      expectedOffsetSeconds: expectedOffsetForWindow(expected, j),
      topModeOffsetSeconds: null,
      withinTolerance: null,
    })),
    error: null,
  };
  if (indexResult.status !== "ok") {
    receipt.status = "skipped";
    receipt.error = "index step failed";
    failed = true;
    caseReceipts.push(receipt);
    continue;
  }
  console.log(`match: ${step.argv.join(" ")}`);
  const result = runStep(step);
  if (result.status !== "ok") {
    receipt.status = "failed";
    receipt.error = result.error;
    failed = true;
    console.error(`match step failed (${step.caseId}): ${result.error}`);
    caseReceipts.push(receipt);
    continue;
  }
  writeFileSync(step.reportPath, JSON.stringify(result.envelope, null, 2) + "\n");
  receipt.status = "ok";
  receipt.reportSha256 = sha256File(step.reportPath);
  const data = result.envelope.data ?? {};
  const pooled = data.pooledOffsetEvidence ?? {};
  receipt.completeness = {
    searchComplete: data.search?.complete ?? null,
    offsetCellScanComplete: pooled.offsetCellScanComplete ?? null,
  };
  const reportedWindows = Array.isArray(pooled.windows) ? pooled.windows : [];
  for (const outcome of receipt.windows) {
    const reported = reportedWindows.find(
      (w) => w.startSeconds === outcome.startSeconds && w.durationSeconds === outcome.durationSeconds,
    );
    const topMode = reported?.modes?.[0];
    if (isFiniteNumber(topMode?.offsetSeconds)) {
      outcome.topModeOffsetSeconds = topMode.offsetSeconds;
      outcome.withinTolerance =
        Math.abs(topMode.offsetSeconds - outcome.expectedOffsetSeconds)
        <= expected.offsetToleranceSeconds;
    }
  }
  caseReceipts.push(receipt);
}

const receipt = {
  schemaVersion: 1,
  runner: "run-source-audio-eval.mjs",
  mode: "execute",
  status: failed ? "failed" : "complete",
  contract: { binaryPath, manifestPath, mediaRoot, outputPath, workDir },
  binaryPresent: true,
  binarySha256: sha256File(binaryPath),
  manifestSha256: sha256File(manifestPath),
  offsetConvention: OFFSET_CONVENTION,
  media,
  indexStep: indexReceipt,
  cases: caseReceipts,
};
mkdirSync(dirname(outputPath), { recursive: true });
writeFileSync(outputPath, JSON.stringify(receipt, null, 2) + "\n");
console.log(`${receipt.status}: receipt written to ${outputPath}`);
process.exit(failed ? 1 : 0);
