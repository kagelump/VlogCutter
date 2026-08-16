// Independent falsification suite for apps/desktop/src/review/order.ts.
//
// This file is owned exclusively by the independent tester for D01-T003. It
// deliberately does NOT import or adapt apps/desktop/src/review/order.test.ts
// or any of its fixtures. Every fixture, id scheme, and assertion below was
// built from the task packet's "Required interface" / "Acceptance oracle" /
// "Independent test charter" sections only.

import { describe, expect, it } from "vitest";
import type { Asset } from "../types";
import { indexOfAsset, moveSelection, nextSelection, orderAssets } from "../review/order";

// A minimal, independently-authored Asset builder. Field values are chosen to
// be self-evidently arbitrary (not mirroring any "sensible camera" fixture)
// so nothing here can be mistaken for a copy of another suite's helper.
function buildClip(fields: Partial<Asset> & { id: string }): Asset {
  return {
    projectId: "proj-independent",
    path: `/vault/${fields.id}`,
    filename: fields.id,
    fingerprint: `fpr-${fields.id}`,
    sizeBytes: 42,
    durationSeconds: 3.5,
    frameRate: 24,
    width: 3840,
    height: 2160,
    codec: "hevc",
    camera: "unspecified",
    capturedAtRaw: null,
    bestTime: null,
    offsetSeconds: 0,
    proxyPath: null,
    proxyRepresentation: null,
    metadataRotationDegrees: 0,
    defaultRotationDegrees: 0,
    rotationDegrees: 0,
    orientationProbed: false,
    originalAvailable: true,
    proxyAvailable: true,
    thumbnailPath: null,
    thumbnailQuality: 0,
    notes: "",
    title: "",
    description: "",
    labels: [],
    removed: false,
    mediaObjectId: null,
    mediaVerificationState: "unlinked",
    available: true,
    revision: 0,
    ...fields,
  };
}

// Deterministic PRNG (mulberry32) so "32 shuffles" is reproducible across CI
// runs without relying on Math.random, and so a failure is reproducible.
function mulberry32(seed: number): () => number {
  let a = seed;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function shuffledCopy<T>(items: T[], rng: () => number): T[] {
  const copy = items.slice();
  for (let i = copy.length - 1; i > 0; i--) {
    const j = Math.floor(rng() * (i + 1));
    [copy[i], copy[j]] = [copy[j], copy[i]];
  }
  return copy;
}

// The charter-mandated fixture: duplicate best_time values, a null best_time,
// a non-ASCII filename, and two assets whose filenames differ only by a
// counter suffix.
const CHARTER_FIXTURE: Asset[] = [
  buildClip({ id: "clip-alpha", bestTime: "2026-02-14T08:00:00Z", filename: "GH010001.MP4" }),
  buildClip({ id: "clip-beta", bestTime: "2026-02-14T08:00:00Z", filename: "GH010002.MP4" }), // duplicate best_time
  buildClip({ id: "clip-gamma", bestTime: null, filename: "旅行_記録_一日目.mov" }), // null best_time + non-ASCII
  buildClip({ id: "clip-delta", bestTime: "2026-02-15T03:22:11Z", filename: "counter_007.mp4" }),
  buildClip({ id: "clip-epsilon", bestTime: "2026-02-15T03:22:11Z", filename: "counter_008.mp4" }), // duplicate best_time, counter-only diff
  buildClip({ id: "clip-zeta", bestTime: "2026-01-01T00:00:00Z", filename: "removed_one.mp4", removed: true }),
  buildClip({ id: "clip-eta", bestTime: null, filename: "removed_two.mp4", removed: true }),
];

describe("independent: orderAssets total + stable across shuffles (charter fixture)", () => {
  it("is total and stable across 32 independent shuffles of the input array", () => {
    const activeIdsInInputOrder = CHARTER_FIXTURE.filter((a) => !a.removed).map((a) => a.id);
    const removedIdsInInputOrder = CHARTER_FIXTURE.filter((a) => a.removed).map((a) => a.id);

    for (let trial = 0; trial < 32; trial++) {
      const rng = mulberry32(1000 + trial);
      const shuffled = shuffledCopy(CHARTER_FIXTURE, rng);

      const activeView = orderAssets(shuffled, { removedVisible: false });
      const removedView = orderAssets(shuffled, { removedVisible: true });

      // Total: every asset appears in exactly one of the two views, and no
      // asset is dropped or duplicated regardless of shuffle order.
      expect(activeView.length + removedView.length).toBe(shuffled.length);
      expect(new Set([...activeView, ...removedView].map((a) => a.id)).size).toBe(CHARTER_FIXTURE.length);

      // The two duplicate-bestTime / counter-suffix assets both survive
      // independently — no implicit de-duplication by bestTime or by
      // filename-prefix similarity.
      expect(activeView.map((a) => a.id)).toEqual(
        expect.arrayContaining(["clip-alpha", "clip-beta", "clip-delta", "clip-epsilon"]),
      );
      // The non-ASCII, null-bestTime asset survives untouched.
      expect(activeView.some((a) => a.id === "clip-gamma" && a.filename === "旅行_記録_一日目.mov")).toBe(true);

      // Stable: calling twice on the SAME (shuffled) input yields the same
      // order both times.
      const again = orderAssets(shuffled, { removedVisible: false });
      expect(again.map((a) => a.id)).toEqual(activeView.map((a) => a.id));

      // orderAssets must preserve whatever relative order it was handed —
      // it must not silently re-sort by bestTime/filename/id. So the
      // active-view order must equal the relative order of active ids as
      // they appear in THIS shuffle, not the original input order (unless
      // this particular shuffle happens to match, which the seeded RNG
      // avoids in the vast majority of the 32 trials).
      const expectedRelativeOrder = shuffled.filter((a) => !a.removed).map((a) => a.id);
      expect(activeView.map((a) => a.id)).toEqual(expectedRelativeOrder);

      const expectedRemovedOrder = shuffled.filter((a) => a.removed).map((a) => a.id);
      expect(removedView.map((a) => a.id)).toEqual(expectedRemovedOrder);
    }

    // Sanity: with the *unshuffled* input, active/removed views match the
    // original fixture's own relative order (oracle case 1).
    expect(orderAssets(CHARTER_FIXTURE, { removedVisible: false }).map((a) => a.id)).toEqual(activeIdsInInputOrder);
    expect(orderAssets(CHARTER_FIXTURE, { removedVisible: true }).map((a) => a.id)).toEqual(removedIdsInInputOrder);
  });
});

describe("independent: acceptance oracle case 1 — removedVisible:false omission + relative order", () => {
  it("omits every removed asset and preserves the relative order of survivors", () => {
    const queue = [
      buildClip({ id: "q1" }),
      buildClip({ id: "q2", removed: true }),
      buildClip({ id: "q3" }),
      buildClip({ id: "q4", removed: true }),
      buildClip({ id: "q5" }),
      buildClip({ id: "q6" }),
    ];
    expect(orderAssets(queue, { removedVisible: false }).map((a) => a.id)).toEqual(["q1", "q3", "q5", "q6"]);
  });
});

describe("independent: acceptance oracle case 2 — stability and non-collapsing", () => {
  it("returns the same order on repeated calls with the same input", () => {
    const queue = [buildClip({ id: "s1" }), buildClip({ id: "s2" }), buildClip({ id: "s3", removed: true })];
    const call1 = orderAssets(queue, { removedVisible: false }).map((a) => a.id);
    const call2 = orderAssets(queue, { removedVisible: false }).map((a) => a.id);
    const call3 = orderAssets(queue, { removedVisible: false }).map((a) => a.id);
    expect(call2).toEqual(call1);
    expect(call3).toEqual(call1);
  });

  it("never collapses two distinct assets into one entry, even with identical field values", () => {
    // Two wholly distinct assets that happen to share every displayed field
    // except id must both survive as two separate entries.
    const identicalTwins = [
      buildClip({ id: "twin-a", bestTime: "2020-05-05T05:05:05Z", filename: "twin.mp4" }),
      buildClip({ id: "twin-b", bestTime: "2020-05-05T05:05:05Z", filename: "twin.mp4" }),
    ];
    const result = orderAssets(identicalTwins, { removedVisible: false });
    expect(result).toHaveLength(2);
    expect(result.map((a) => a.id)).toEqual(["twin-a", "twin-b"]);
  });
});

describe("independent: indexOfAsset", () => {
  const ordered = [buildClip({ id: "n1" }), buildClip({ id: "n2" }), buildClip({ id: "n3" })];

  it("finds a present id at its correct index", () => {
    expect(indexOfAsset(ordered, "n2")).toBe(1);
  });

  it("returns -1 for an id not present", () => {
    expect(indexOfAsset(ordered, "does-not-exist")).toBe(-1);
  });

  it("returns -1 for a null id", () => {
    expect(indexOfAsset(ordered, null)).toBe(-1);
  });

  it("returns -1 on an empty ordered list", () => {
    expect(indexOfAsset([], "n1")).toBe(-1);
  });
});

describe("independent: acceptance oracle cases 3-5 — moveSelection boundaries", () => {
  const queue = [
    buildClip({ id: "boundary-first" }),
    buildClip({ id: "boundary-mid-a" }),
    buildClip({ id: "boundary-mid-b" }),
    buildClip({ id: "boundary-last" }),
  ];

  it("case 3: moveSelection(ordered, firstId, -1) returns firstId (no wrap to last)", () => {
    const result = moveSelection(queue, "boundary-first", -1);
    expect(result).toBe("boundary-first");
    expect(result).not.toBeNull();
    expect(result).not.toBe("boundary-last");
  });

  it("case 4: moveSelection(ordered, lastId, +1) returns lastId (no wrap to first)", () => {
    const result = moveSelection(queue, "boundary-last", 1);
    expect(result).toBe("boundary-last");
  });

  it("case 5: moveSelection(ordered, null, +1) returns the first asset's id", () => {
    expect(moveSelection(queue, null, 1)).toBe("boundary-first");
  });
});

describe("independent: charter — moveSelection never returns an id absent from ordered", () => {
  const queue = [
    buildClip({ id: "p0" }),
    buildClip({ id: "p1" }),
    buildClip({ id: "p2" }),
    buildClip({ id: "p3" }),
    buildClip({ id: "p4" }),
  ];
  const idSet = new Set(queue.map((a) => a.id));
  const deltas = [-3, -2, -1, 0, 1, 2, 3];
  const startingPositions: (string | null)[] = [null, ...queue.map((a) => a.id)];

  for (const start of startingPositions) {
    for (const delta of deltas) {
      it(`delta=${delta} from start=${start ?? "null"} stays within ordered`, () => {
        const result = moveSelection(queue, start, delta);
        expect(result).not.toBeNull();
        expect(idSet.has(result as string)).toBe(true);
      });
    }
  }

  it("degenerate case: an empty ordered list returns the current id unchanged (documented exception)", () => {
    expect(moveSelection([], "anything", 1)).toBe("anything");
    expect(moveSelection([], null, -2)).toBeNull();
  });
});

describe("independent: acceptance oracle case 6 — nextSelection", () => {
  it("returns the same id when the asset survives a reorder", () => {
    const before = [buildClip({ id: "r1" }), buildClip({ id: "r2" }), buildClip({ id: "r3" }), buildClip({ id: "r4" })];
    const survivorId = "r3";
    const reordered = [before[3], before[0], before[2], before[1]]; // r4, r1, r3, r2
    expect(nextSelection(reordered, survivorId)).toBe(survivorId);
  });

  it("falls back to the first surviving asset in `ordered` when the previous selection is gone", () => {
    // Prior queue (conceptually): g1, g2, g3(selected, about to be removed), g4, g5.
    // After g3 leaves, the corrected contract (packet commit 47c1c1c,
    // 2026-07-30) is: fall back to the first surviving asset in `ordered` by
    // array order, matching the app's existing resync fallback at App.tsx's
    // `setSelectedId(filteredAssets[0]?.id ?? "")` (~line 399). This is
    // deliberately NOT "nearest by prior position" — a two-argument
    // (ordered, previousId) signature over only the post-change list has no
    // way to recover an absent id's prior adjacency, so g2/g4 being closer to
    // g3's old slot than g1 is irrelevant; the first surviving entry, g1,
    // wins regardless of where g3 used to sit.
    const after = [
      buildClip({ id: "g1" }),
      buildClip({ id: "g2" }),
      buildClip({ id: "g4" }),
      buildClip({ id: "g5" }),
    ];
    const result = nextSelection(after, "g3");
    expect(result).toBe("g1");
  });

  it("returns null when nothing survives", () => {
    expect(nextSelection([], "anything")).toBeNull();
    expect(nextSelection([], null)).toBeNull();
  });
});
