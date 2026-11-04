#!/usr/bin/env node
// Deterministic public synthetic audio fixtures for source-audio CI.
//
// Produces, into --out <dir>:
//   movie.wav              synthetic "movie" soundtrack (12.0 s)
//   recording-continuous.wav  the movie heard at a known delay plus seeded
//                          room noise/mixing (15.0 s)
//   synthetic-ci-v1.json   runner manifest with expected offsets and digests
//
// Determinism contract (byte-identical across runs and machines):
//   * fixed seeds for every pseudo-random stream (xorshift32 defined below);
//   * sample synthesis uses only IEEE-754 basic arithmetic (+, -, *, /) and
//     comparisons. No Math.sin/cos/pow/exp: those are implementation-defined
//     and may differ across engines/platforms. Quantization and WAV packing
//     are specified exactly.
//
// Offset sign convention (frozen design):
//   movieElapsedSeconds = recordingSecondsWithinEpoch + offsetSeconds
// The movie begins at recording time 3.0 s here, so the expected offset is
//   0.0 = 3.0 + offsetSeconds  =>  offsetSeconds = -3.0
// (negative: the movie position is earlier than the recording position).
//
// The fixtures are original synthetic content; no private or RC6-derived
// audio is reproduced.

import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";

const RATE = 11025; // matches the landmark recipe's internal sample rate
const MOVIE_SECONDS = 12;
const ROOM_SECONDS = 3; // room tone before the movie is heard
const EVENT_SECONDS = 0.4;
const RAMP_SAMPLES = 220; // 20 ms linear envelope per event (anti-click)

const MOVIE_SAMPLES = RATE * MOVIE_SECONDS;
const RECORDING_SAMPLES = RATE * (ROOM_SECONDS + MOVIE_SECONDS);
const EVENT_SAMPLES = Math.floor(RATE * EVENT_SECONDS);

const SEED_MOVIE_CHORDS = 0x5eed10c1;
const SEED_MOVIE_NOISE = 0x1a2b3c4d;
const SEED_ROOM = 0x7f8e9d0c;
const SEED_RECORDING_MIX = 0x2468ace0;

const OFFSET_CONVENTION =
  "movieElapsedSeconds = recordingSecondsWithinEpoch + offsetSeconds";
const EXPECTED_OFFSET_SECONDS = -3.0;

function usage() {
  console.error("Usage: generate-synthetic-fixtures.mjs --out <dir>");
  process.exit(2);
}

const { values } = parseArgs({
  options: { out: { type: "string" } },
  allowPositionals: false,
});
if (!values.out) usage();

// xorshift32 over uint32; returns doubles in [0, 1).
function makePrng(seed) {
  let s = seed >>> 0;
  if (s === 0) s = 0x9e3779b9;
  return () => {
    s ^= (s << 13) >>> 0;
    s >>>= 0;
    s ^= s >>> 17;
    s ^= (s << 5) >>> 0;
    s >>>= 0;
    return s / 4294967296;
  };
}

// Sample quantization: clamp to [-1, 1], round half away from zero, i16.
function quantize(x) {
  const c = x > 1 ? 1 : x < -1 ? -1 : x;
  const v = c >= 0 ? Math.floor(c * 32767 + 0.5) : -Math.floor(-c * 32767 + 0.5);
  return v;
}

function generateMovie() {
  const chords = makePrng(SEED_MOVIE_CHORDS);
  const noise = makePrng(SEED_MOVIE_NOISE);
  const samples = new Int16Array(MOVIE_SAMPLES);
  for (let event = 0; event * EVENT_SAMPLES < MOVIE_SAMPLES; event += 1) {
    const start = event * EVENT_SAMPLES;
    const length = Math.min(EVENT_SAMPLES, MOVIE_SAMPLES - start);
    const oscillators = [];
    for (let j = 0; j < 3; j += 1) {
      oscillators.push({
        freq: 220 + Math.floor(chords() * 4180), // integer Hz in [220, 4400]
        amp: [0.45 + chords() * 0.15, 0.3 + chords() * 0.1, 0.2 + chords() * 0.1][j],
        phase: chords(),
      });
    }
    for (let i = 0; i < length; i += 1) {
      let env = 1;
      if (i < RAMP_SAMPLES) env = i / RAMP_SAMPLES;
      if (length - 1 - i < RAMP_SAMPLES) {
        env = Math.min(env, (length - 1 - i) / RAMP_SAMPLES);
      }
      let v = 0;
      for (const o of oscillators) {
        o.phase += o.freq / RATE;
        if (o.phase >= 1) o.phase -= 1;
        v += o.amp * (o.phase < 0.5 ? 1 : -1);
      }
      v = v * env * 0.35 + (noise() * 2 - 1) * 0.02;
      samples[start + i] = quantize(v);
    }
  }
  return samples;
}

function generateRecording(movie) {
  const room = makePrng(SEED_ROOM);
  const mix = makePrng(SEED_RECORDING_MIX);
  const samples = new Int16Array(RECORDING_SAMPLES);
  const lead = ROOM_SECONDS * RATE;
  for (let i = 0; i < RECORDING_SAMPLES; i += 1) {
    let v = (room() * 2 - 1) * 0.03 + (mix() * 2 - 1) * 0.01;
    if (i >= lead) v += (movie[i - lead] / 32768) * 0.8;
    samples[i] = quantize(v);
  }
  return samples;
}

function wavBytes(samples, rate) {
  const dataBytes = samples.length * 2;
  const bytes = Buffer.alloc(44 + dataBytes);
  bytes.write("RIFF", 0, "ascii");
  bytes.writeUInt32LE(36 + dataBytes, 4);
  bytes.write("WAVE", 8, "ascii");
  bytes.write("fmt ", 12, "ascii");
  bytes.writeUInt32LE(16, 16); // PCM fmt chunk size
  bytes.writeUInt16LE(1, 20); // PCM
  bytes.writeUInt16LE(1, 22); // mono
  bytes.writeUInt32LE(rate, 24);
  bytes.writeUInt32LE(rate * 2, 28); // byte rate
  bytes.writeUInt16LE(2, 32); // block align
  bytes.writeUInt16LE(16, 34); // bits per sample
  bytes.write("data", 36, "ascii");
  bytes.writeUInt32LE(dataBytes, 40);
  for (let i = 0; i < samples.length; i += 1) {
    bytes.writeInt16LE(samples[i], 44 + i * 2);
  }
  return bytes;
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function writeJson(path, value) {
  writeFileSync(path, JSON.stringify(value, null, 2) + "\n");
}

const movie = generateMovie();
const recording = generateRecording(movie);
const movieWav = wavBytes(movie, RATE);
const recordingWav = wavBytes(recording, RATE);
const movieSha = sha256(movieWav);
const recordingSha = sha256(recordingWav);

const movieClock = {
  movieDigest: movieSha,
  timeBaseNumerator: 1,
  timeBaseDenominator: RATE,
  originPts: 0,
  durationPts: MOVIE_SAMPLES,
};

const captureClock = {
  clockId: "synthetic-ci-v1-continuous",
  sampleRate: RATE,
  firstSample: 0,
  lastSample: RECORDING_SAMPLES,
  monotonicOriginNs: 0,
  monotonicToleranceNs: 0,
};

const syncPolicy = {
  minimumConfidence: 0.8,
  maxAnchorGapSeconds: 15.0,
  maxExtrapolationSeconds: 0.0,
  maxDriftPpm: 2000.0,
  residualToleranceSeconds: 0.1,
  unobservedUncertaintyPpm: 5000.0,
};

// Alignment ("policy") values are the core defaults, spelled out so the
// example is complete rather than placeholder JSON.
const alignmentPolicy = {
  minimumTileAnchors: 4,
  minimumTileSeconds: 1,
  minimumFrequencyDiversity: 4,
  minimumSegmentTiles: 6,
  minimumSegmentAnchors: 10,
  minimumSegmentSeconds: 6,
  scoreBaselinePerTile: 2.1,
  segmentChangePenalty: 3.0,
  ambiguityMargin: 0.75,
  maxDriftPpm: 2000.0,
  maximumGapSeconds: 2.0,
  gapPenaltyPerSecond: 0.75,
  anchorStepSeconds: 5.0,
};

const pooledOffsetWindows = [
  { startSeconds: 3, durationSeconds: 10 },
  { startSeconds: 4, durationSeconds: 10 },
];

// Request shape per the preserved CLI request schema (pooledOffsetWindows /
// pooledOffsetModeLimit included; see README "Request shape").
const request = {
  captureClockId: "synthetic-ci-v1-continuous",
  policy: alignmentPolicy,
  searchPolicy: {
    independenceCaps: true,
    rarityWeighting: true,
    saturationDownweighting: true,
  },
  diagnosticOffsetEvidencePerTile: 12,
  pooledOffsetWindows,
  pooledOffsetModeLimit: 8,
  sync: {
    schemaVersion: 1,
    sessionId: "synthetic-ci-v1-continuous",
    // The synthetic set has no review target; per existing core fixture
    // convention targetDigest reuses the movie digest. Digests in this
    // manifest are SHA-256 of the generated file bytes.
    targetDigest: movieSha,
    movie: movieClock,
    captureClocks: [captureClock],
    anchors: [],
    boundaries: [],
    policy: syncPolicy,
  },
};

const manifest = {
  schemaVersion: 1,
  fixtureSet: "synthetic-ci-v1",
  generator: "generate-synthetic-fixtures.mjs",
  purpose:
    "deterministic public synthetic audio for CI: a synthetic movie soundtrack and one recording of that movie heard at a known delay behind seeded room noise/mixing",
  offsetConvention: OFFSET_CONVENTION,
  offsetSignNote:
    "negative offsetSeconds means the movie position is earlier than the recording position; the movie is heard from its start at recording time 3.0 s here",
  digestConvention:
    "sha256 hex of the generated file bytes; targetDigest reuses the movie digest (no review target exists for the synthetic set), and the matcher reports its own BLAKE3 recordingDigest in match output",
  determinism:
    "fixed xorshift32 seeds and IEEE-754 basic arithmetic only; regenerating with node produces byte-identical files",
  movie: {
    path: "movie.wav",
    sha256: movieSha,
    sampleRate: RATE,
    sampleCount: MOVIE_SAMPLES,
    durationSeconds: MOVIE_SECONDS,
    clock: movieClock,
  },
  indexRequest: {
    movie: movieClock,
    fftSize: 512,
    hashesPerSecond: 100,
    fanout: 3,
  },
  cases: [
    {
      caseId: "continuous-movie-at-recording-3s",
      description:
        "the full movie is heard from its start at recording time 3.0 s behind 3.0 s of seeded room noise; every requested window expects offsetSeconds = 0.0 - 3.0 = -3.0",
      recording: {
        path: "recording-continuous.wav",
        sha256: recordingSha,
        sampleRate: RATE,
        firstSample: 0,
        lastSample: RECORDING_SAMPLES,
        captureClockId: "synthetic-ci-v1-continuous",
      },
      request,
      expected: {
        offsetSeconds: EXPECTED_OFFSET_SECONDS,
        offsetToleranceSeconds: 0.1,
        derivation:
          "movieElapsedSeconds = recordingSecondsWithinEpoch + offsetSeconds; the movie starts (0.0) at recording time 3.0, so offsetSeconds = 0.0 - 3.0 = -3.0",
        windows: pooledOffsetWindows.map((w) => ({
          ...w,
          expectedOffsetSeconds: EXPECTED_OFFSET_SECONDS,
        })),
      },
    },
  ],
};

mkdirSync(values.out, { recursive: true });
writeFileSync(join(values.out, "movie.wav"), movieWav);
writeFileSync(join(values.out, "recording-continuous.wav"), recordingWav);
writeJson(join(values.out, "synthetic-ci-v1.json"), manifest);

console.log(
  JSON.stringify(
    {
      out: values.out,
      movie: { path: "movie.wav", sha256: movieSha, bytes: movieWav.length },
      recording: {
        path: "recording-continuous.wav",
        sha256: recordingSha,
        bytes: recordingWav.length,
      },
      manifest: { path: "synthetic-ci-v1.json", fixtureSet: manifest.fixtureSet },
      expectedOffsetSeconds: EXPECTED_OFFSET_SECONDS,
      offsetConvention: OFFSET_CONVENTION,
    },
    null,
    2,
  ),
);
