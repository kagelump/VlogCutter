/**
 * The browser preview's demo project.
 *
 * This is *scenario declaration only*: every value here goes through the same
 * `ScenarioBuilder` a CUJ uses, and is served afterwards by the same command
 * handlers. There is no second fake backend — the preview and the CUJ suite are
 * two front doors onto `DesktopHarnessState`.
 *
 * What may live here that may not live in a CUJ: richer demo data. A scenario
 * built for a test states the least that makes its question answerable; this one
 * states enough to walk the whole app.
 */

import { desktopScenario, type ScenarioBuilder } from "../test/harness/scenario";
import { buildContradiction, buildDiagnostic } from "../test/harness/chronologyFixtures";
import type { ChronologyImpact, ClockEpoch, EditPlanRevision, TripPhase } from "../chronologyTypes";

const PROJECT = "Hokkaido field test";
const PROJECT_ID = "project_hokkaido_field_test";

/** The card in the slot. Three of its six clips are already in the project. */
export const CARD_MOUNT = "/Volumes/DJI_SD";
export const ARCHIVE_MOUNT = "/Volumes/nas/VideoArchive";

const SONY = ["asset_sony_a7c_ii_001", "asset_sony_a7c_ii_002", "asset_sony_a7c_ii_003", "asset_sony_a7c_ii_004"];
const IPHONE = ["asset_apple_iphone_17_pro_001", "asset_apple_iphone_17_pro_002"];
const INSTA = ["asset_insta360_x5_001", "asset_insta360_x5_002"];
const DJI = ["asset_dji_pocket_3_001", "asset_dji_pocket_3_002", "asset_dji_pocket_3_003"];

/**
 * The order the solver placed these clips in, and the times it placed them at.
 *
 * Declared, exactly as a CUJ declares it: `rcdvlog-core` owns the solver, and a
 * preview that computed its own order would be a machine for producing false
 * confidence. `CHRONOLOGY_START` and `CHRONOLOGY_SPACING` are what
 * `buildChronologyOverview` turns into per-clip trip times, so the phase
 * anchors below can name the same instants.
 */
const CHRONOLOGY_ORDER = [
  SONY[0], SONY[1], IPHONE[0], INSTA[0], SONY[2], IPHONE[1], SONY[3], INSTA[1], ...DJI,
];
const CHRONOLOGY_START = "2026-02-08T00:14:08.000Z";
const CHRONOLOGY_SPACING = 600;
const INSTA_CORRECTION: ClockEpoch = {
  id: "epoch_preview_insta",
  captureSequenceId: "sequence_insta",
  startCounter: 0,
  endCounter: null,
  offsetSeconds: 15 * 60,
  confidence: 1,
  evidence: ["user_set"],
  label: "Insta360 clock was fifteen minutes slow",
};
const INSTA_CORRECTION_IMPACT: ChronologyImpact = {
  changes: [
    { assetId: INSTA[0], before: placedAt(3), after: new Date(Date.parse(placedAt(3)) + 15 * 60_000).toISOString(), beforePosition: 3, afterPosition: 4 },
    { assetId: SONY[2], before: placedAt(4), after: placedAt(4), beforePosition: 4, afterPosition: 3 },
    { assetId: INSTA[1], before: placedAt(7), after: new Date(Date.parse(placedAt(7)) + 15 * 60_000).toISOString(), beforePosition: 7, afterPosition: 8 },
    { assetId: DJI[0], before: placedAt(8), after: placedAt(8), beforePosition: 8, afterPosition: 7 },
  ],
  contradictions: [],
  blocked: false,
};

/** The trip time the overview gives the clip at `index` in the declared order. */
function placedAt(index: number): string {
  return new Date(Date.parse(CHRONOLOGY_START) + index * CHRONOLOGY_SPACING * 1_000).toISOString();
}

function phase(spec: {
  id: string;
  label: string;
  kind: TripPhase["kind"];
  from: number;
  to: number;
  timezone: string;
}): TripPhase {
  return {
    id: spec.id,
    label: spec.label,
    kind: spec.kind,
    startAnchorAssetId: CHRONOLOGY_ORDER[spec.from],
    endAnchorAssetId: CHRONOLOGY_ORDER[spec.to],
    startTripTime: placedAt(spec.from),
    endTripTime: placedAt(spec.to),
    timezone: spec.timezone,
    originTimezone: null,
    destinationTimezone: null,
    displayMode: "local",
  };
}

/**
 * The parts of the overview an edit does not change: which cameras shot the
 * trip, and which stretch of it each phase covers. Stated once so a declared
 * post-edit outcome keeps them rather than silently emptying the ribbon.
 */
function chronologyBase(order: string[]) {
  return {
    order,
    startAt: CHRONOLOGY_START,
    spacingSeconds: CHRONOLOGY_SPACING,
    timezone: "Asia/Tokyo",
    sequences: [
      // Deliberately interleaved in solved order. The browser tier uses this to
      // prove the two card identities become separate, non-overlapping runs
      // inside one Sony lane instead of first-to-last bands covering each other.
      { id: "sequence_sony_a", cameraKey: "Sony A7C II", label: "Sony A7C II · card A", volumeHint: "Travel/A001", filenamePrefix: "SONY", assetIds: [SONY[0], SONY[2]] },
      { id: "sequence_sony_b", cameraKey: "Sony A7C II", label: "Sony A7C II · card B", volumeHint: "Travel/A002", filenamePrefix: "SONY", assetIds: [SONY[1], SONY[3]] },
      { id: "sequence_iphone", cameraKey: "Apple iPhone 17 Pro", label: "iPhone 17 Pro", volumeHint: "Travel", filenamePrefix: "APPLE", assetIds: IPHONE },
      { id: "sequence_insta", cameraKey: "Insta360 X5", label: "Insta360 X5", volumeHint: "Travel", filenamePrefix: "INSTA360", assetIds: INSTA },
      { id: "sequence_dji", cameraKey: "DJI Pocket 3", label: "DJI Pocket 3", volumeHint: "DJI_SD", filenamePrefix: "DJI", assetIds: DJI },
    ],
    phases: [
      phase({ id: "phase_otaru", label: "Otaru", kind: "location", from: 0, to: 3, timezone: "Asia/Tokyo" }),
      phase({ id: "phase_sapporo", label: "Sapporo morning", kind: "location", from: 4, to: 7, timezone: "Asia/Tokyo" }),
      phase({ id: "phase_niseko", label: "Niseko", kind: "location", from: 8, to: 10, timezone: "Asia/Tokyo" }),
    ],
  };
}

/** An edit plan already on its second revision, so the panel has a history. */
function editPlanRevisions(): EditPlanRevision[] {
  const operations = [
    { assetId: SONY[0], sourceInSeconds: 4.2, sourceOutSeconds: 9.8, rationale: "The train arrives" },
    { assetId: IPHONE[0], sourceInSeconds: 0.5, sourceOutSeconds: 5.1, rationale: "Walk into the arcade" },
    { assetId: SONY[2], sourceInSeconds: 2.0, sourceOutSeconds: 11.4, rationale: "Market stalls opening" },
  ];
  const revision = (revisionId: string, parentRevisionId: string | null, count: number): EditPlanRevision => ({
    revisionId,
    projectId: PROJECT_ID,
    scriptRevisionId: null,
    parentRevisionId,
    active: parentRevisionId !== null,
    operations: operations.slice(0, count).map((operation, position) => ({
      operationId: `operation_${revisionId}_${position + 1}`,
      position,
      ...operation,
    })),
    createdAt: revisionCreatedAt(parentRevisionId === null ? 0 : 1),
  });
  return [revision("revision_1", null, 2), revision("revision_2", "revision_1", 3)];
}

function revisionCreatedAt(index: number): string {
  return new Date(Date.parse("2026-08-09T10:00:00.000Z") + index * 3_600_000).toISOString();
}

/**
 * The preview's scenario.
 *
 * Exported as a builder rather than as a harness so the demo can be inspected,
 * extended, or reused by a test that wants to prove the preview and the CUJ
 * suite are served by one implementation.
 */
export function previewScenario(): ScenarioBuilder {
  const scenario = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project(PROJECT, { id: PROJECT_ID, createdAt: "2026-08-01T09:00:00.000Z" })
    // Plausible weights for a real trip, so the storage panel and the delete
    // confirmation have something honest-looking to lay out (#16).
    .storageUsage(
      {
        proxies: { bytes: 14_800_000_000, fileCount: 312 },
        thumbnails: { bytes: 61_000_000, fileCount: 312 },
        audio: { bytes: 940_000_000, fileCount: 312 },
        framePacks: { bytes: 2_300_000_000, fileCount: 4_800 },
        editPreviews: { bytes: 180_000_000, fileCount: 6 },
        importPreviews: { bytes: 24_000_000, fileCount: 243 },
        analysisModels: { bytes: 3_100_000_000, fileCount: 4 },
      },
      2_240_000,
    )
    .camera("Sony A7C II", {
      clips: 4,
      startAt: "2026-02-08T00:14:08.000Z",
      spacingSeconds: 780,
      durationSeconds: 18.4,
      withProxy: true,
    })
    .camera("Apple iPhone 17 Pro", {
      clips: 2,
      startAt: "2026-02-08T00:27:19.000Z",
      spacingSeconds: 1_500,
      durationSeconds: 8.2,
      extension: "MOV",
      withProxy: 1,
    })
    .camera("Insta360 X5", {
      clips: 2,
      startAt: "2026-02-08T00:38:55.000Z",
      spacingSeconds: 1_200,
      durationSeconds: 31.7,
      withProxy: false,
    })
    // The DJI clips came off this card on an earlier import, which is what makes
    // the second insertion — three more clips on the same card — a real flow
    // rather than a first-run one.
    .cardInserted("DJI_SD", {
      clips: 6,
      newClips: 3,
      camera: "DJI Pocket 3",
      cameraHint: "DJI Pocket 3",
      mountPath: CARD_MOUNT,
      totalBytes: 80_000_000_000,
    })
    .withProxy(DJI[0], DJI[1])
    // Every clip but the last DJI one has a frame. The gap is deliberate: the
    // chronology canvas has to stay readable on a project mid-generation, and
    // the preview is where that fallback is easiest to eyeball.
    .withThumbnail(...SONY, ...IPHONE, ...INSTA, DJI[0], DJI[1])
    .archiveRoot("NAS Video", { localMountPath: ARCHIVE_MOUNT, availableBytes: 1_200_000_000_000 });

  return scenario
    .withSummary(
      SONY[0],
      "Train pulls into Otaru station",
      "A local train slows along the platform as an announcement plays; the camera pans left to follow it past the station clock.",
    )
    .withLabels(SONY[0], "arrival", "establishing")
    .withNote(SONY[0], "First clean view of the station clock.")
    .withTranscript(SONY[0], { text: "次は小樽、小樽です。", language: "ja", speechSeconds: 7.4 })
    .withAnalysis(SONY[0], {
      aiLabels: ["station", "arrival"],
      cameraMovement: "pan",
      ocrText: ["小樽駅", "OTARU"],
      transcriptUsed: true,
    })
    .withSummary(IPHONE[0], "Handheld walk past shuttered stalls", "Unsteady handheld shot down a covered arcade.")
    .withAnalysis(IPHONE[0], { usable: false, unusableReason: "blurry", cameraMovement: "handheld" })
    .withTranscript(IPHONE[0])
    .withLabels(SONY[1], "train")
    .withNote(SONY[2], "Good transition into the market.")
    .withLabels(SONY[2], "market", "transition")
    // One clip's original is on a volume that is not mounted. Its proxy keeps it
    // reviewable, which is the whole point of making proxies — and it gives the
    // diagnostics surface something true to say.
    .offlineOriginal(SONY[3])
    .removed(INSTA[1])
    .diagnostic(
      buildDiagnostic({
        severity: "warning",
        kind: "media.volume_offline",
        message: "“Travel” is not mounted, so 1 original cannot be read.",
        volumeHint: "Travel",
        assetIds: [SONY[3]],
      }),
    )
    .chronology({
      ...chronologyBase(CHRONOLOGY_ORDER),
      contradictions: [
        buildContradiction({
          kind: "interval_conflict",
          strength: "soft",
          assetIds: [INSTA[0], SONY[2]],
          message: "The Insta360 clock runs about fifteen minutes behind the Sony.",
          evidence: ["camera clock comparison"],
        }),
      ],
    })
    // Correcting the Insta360's clock is the one edit the preview has a declared
    // outcome for: both Insta360 clips move after their nearby comparison, and the
    // contradiction goes away. The sequences and phases come along unchanged,
    // because a clock correction does not dissolve them.
    .impactOnClockPreview("sequence_insta", {
      epoch: INSTA_CORRECTION,
      factsToken: "facts-preview-insta",
      impact: INSTA_CORRECTION_IMPACT,
    })
    .whenClockEpochApplied("sequence_insta")
    .thenChronology(
      chronologyBase([SONY[0], SONY[1], IPHONE[0], SONY[2], INSTA[0], IPHONE[1], SONY[3], DJI[0], INSTA[1], DJI[1], DJI[2]]),
    )
    .editPlanValidation("revision_2", {
      findings: [
        {
          kind: "clip",
          severity: "warning",
          code: "source_range_overruns_clip",
          position: 2,
          assetId: SONY[2],
          message: "This range ends after the clip does. Trim it before exporting.",
        },
      ],
    })
    .tap((state) => {
      const revisions = editPlanRevisions();
      state.editPlans.revisions = revisions;
      state.editPlans.active = revisions.find((revision) => revision.active) ?? null;
      // The preview has no loopback media origin to hand out: nothing is serving
      // these bytes, and a URL that would never answer is worse than none. Real
      // playback stays a native acceptance item (`MEDIA_DELIVERY.md`).
      state.mediaOrigin = null;
    })
    // Native dialogs do not exist in a browser, so the preview declares what the
    // user picks, in order — the card first, then a home for the archive. Once
    // they run out, a dialog reads as dismissed.
    .dialogAnswers(CARD_MOUNT, ARCHIVE_MOUNT, CARD_MOUNT)
    .selectedClip(SONY[0]);
}

/**
 * A project at the scale the product is designed for.
 *
 * AGENTS.md §1 sets it: thousands of mostly 4K/60 clips across about five
 * cameras. Nothing here is staged for legibility — there is no card in the
 * slot, no chronology problem, no edit plan — because the only question this
 * scenario exists to answer is what the two review surfaces cost to mount when
 * a project is that big. Reached with `?clips=3000` in the preview.
 */
export function scaleScenario(clips: number): ScenarioBuilder {
  const cameras = ["Sony A7C II", "DJI Pocket 3", "Apple iPhone 17 Pro", "Insta360 X5", "Sony FX3"];
  const share = Math.floor(clips / cameras.length);
  const scenario = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project(`${PROJECT} (${clips} clips)`, { id: PROJECT_ID, createdAt: "2026-08-01T09:00:00.000Z" });

  cameras.forEach((camera, index) => {
    // The remainder rides on the last camera, so the total is exactly `clips`.
    const count = index === cameras.length - 1 ? clips - share * (cameras.length - 1) : share;
    if (count <= 0) return;
    scenario.camera(camera, {
      clips: count,
      // Interleaved rather than blocked, which is what a real trip looks like
      // and what makes the ribbon's lanes worth windowing.
      startAt: new Date(Date.parse("2026-02-08T00:00:00.000Z") + index * 120_000).toISOString(),
      spacingSeconds: 600,
      durationSeconds: 18.4,
      withProxy: true,
    });
  });

  return scenario.tap((state) => {
    state.mediaOrigin = null;
  });
}

/**
 * An isolated synthetic scenario for the canonical review-required transcript
 * display. This is reached only with the explicit `?review=required` preview
 * switch; it changes no default demo data and proves display only.
 */
export function reviewRequiredScenario(): ScenarioBuilder {
  const assetId = "asset_sony_a7c_ii_001";
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Synthetic transcript review fixture", {
      id: "project_transcript_review_fixture",
      createdAt: "2026-08-01T09:00:00.000Z",
    })
    .camera("Sony A7C II", {
      clips: 1,
      startAt: "2026-02-08T00:14:08.000Z",
      durationSeconds: 18.4,
      withProxy: true,
    })
    .withTranscript(assetId, {
      status: "review_required",
      outcomeReason: "quality_evidence_incomplete",
      language: "en",
      speechSeconds: 6.5,
      meanTokenProbability: null,
      repetitionRatio: null,
      segmentMeanTokenProbability: null,
      segments: [
        { startMs: 1_200, text: "Synthetic candidate: the train slows along the platform." },
        { startMs: 6_500, text: "Synthetic candidate: someone asks where the market is." },
      ],
    })
    .selectedClip(assetId);
}
