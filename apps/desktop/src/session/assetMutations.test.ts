/**
 * The save state machine, with nothing rendered.
 *
 * What used to be spread across 39 async functions in `App` is one object, so
 * the questions that mattered and could not be asked below the full-shell level
 * — what survives a rejection, what a retry sends, what a second submit does —
 * are asked here directly.
 */

import { describe, expect, it, vi } from "vitest";
import { AssetMutationLifecycle, type MutationSnapshot } from "./assetMutations";
import type { Asset, AssetPatch } from "../types";

function asset(overrides: Partial<Asset> = {}): Asset {
  return {
    id: "asset_1",
    projectId: "project_1",
    path: "/Volumes/Travel/SONY_001.MP4",
    filename: "SONY_001.MP4",
    camera: "Sony A7C II",
    notes: "",
    title: "",
    description: "",
    labels: [],
    removed: false,
    rotationDegrees: 0,
    defaultRotationDegrees: 0,
    ...overrides,
  } as unknown as Asset;
}

interface Harness {
  lifecycle: AssetMutationLifecycle;
  applied: Asset[];
  errors: string[];
  snapshots: MutationSnapshot[];
  update: ReturnType<typeof vi.fn>;
  /** What the last snapshot says. */
  state(): MutationSnapshot;
}

function lifecycleFor(update: (assetId: string, patch: AssetPatch) => Promise<Asset>): Harness {
  const applied: Asset[] = [];
  const errors: string[] = [];
  const snapshots: MutationSnapshot[] = [];
  const spy = vi.fn(update);
  const lifecycle = new AssetMutationLifecycle({
    update: spy,
    applyAsset: (next) => applied.push(next),
    onError: (message) => errors.push(message),
    onChange: (snapshot) => snapshots.push(snapshot),
  });
  return { lifecycle, applied, errors, snapshots, update: spy, state: () => lifecycle.snapshot() };
}

/** The backend accepting the write and answering with what it stored. */
function accepts() {
  return lifecycleFor(async (assetId, patch) => asset({ id: assetId, ...patch }));
}

function refuses(message = "database is locked") {
  return lifecycleFor(async () => {
    throw new Error(message);
  });
}

describe("a write the backend accepts", () => {
  it("shows the change before the round trip and the stored value after it", async () => {
    const harness = accepts();
    const result = await harness.lifecycle.submit(asset(), { labels: ["arrival"] });

    expect(result).toBe("saved");
    // Optimistic first, then whatever the backend actually stored.
    expect(harness.applied.map((entry) => entry.labels)).toEqual([["arrival"], ["arrival"]]);
    expect(harness.state().saveState).toBe("saved");
    expect(harness.errors).toEqual([]);
  });

  it("sends the whole patch, not only the changed field", async () => {
    const harness = accepts();
    await harness.lifecycle.submit(asset({ notes: "snow", labels: ["arrival"] }), { rotationDegrees: 90 });

    expect(harness.update).toHaveBeenCalledWith("asset_1", {
      notes: "snow",
      labels: ["arrival"],
      removed: false,
      rotationDegrees: 90,
    });
  });
});

describe("a write the backend refuses", () => {
  it("rolls the optimistic change back, because a refused label is not a label", async () => {
    const harness = refuses();
    const original = asset();
    const result = await harness.lifecycle.submit(original, { labels: ["arrival"] });

    expect(result).toBe("failed");
    expect(harness.applied.map((entry) => entry.labels)).toEqual([["arrival"], []]);
    expect(harness.state().saveState).toBe("idle");
    expect(harness.errors).toEqual(["Error: database is locked"]);
  });

  it("keeps a note draft on screen and marks it unsaved, because prose is not disposable", async () => {
    const harness = refuses();
    const result = await harness.lifecycle.saveNotes(asset({ notes: "The light turns about here" }));

    expect(result).toBe("failed");
    expect(harness.applied.at(-1)?.notes).toBe("The light turns about here");
    expect(harness.state().saveState).toBe("unsaved");
    expect(harness.lifecycle.draftFor("asset_1")).toEqual({
      assetId: "asset_1",
      notes: "The light turns about here",
      error: "Error: database is locked",
    });
  });

  it("does not report the failed clip's state beside another clip", async () => {
    const harness = refuses();
    await harness.lifecycle.submit(asset({ id: "asset_1" }), { labels: ["arrival"] });

    expect(harness.lifecycle.saveStateFor("asset_1")).toBe("idle");
    expect(harness.lifecycle.saveStateFor("asset_2")).toBe("idle");
  });
});

describe("retrying a retained draft", () => {
  it("sends the same text again and clears the mark when it lands", async () => {
    let refuse = true;
    const harness = lifecycleFor(async (assetId, patch) => {
      if (refuse) throw new Error("database is locked");
      return asset({ id: assetId, ...patch });
    });
    const draft = asset({ notes: "The light turns about here" });

    await harness.lifecycle.saveNotes(draft);
    expect(harness.lifecycle.saveStateFor("asset_1")).toBe("unsaved");

    refuse = false;
    // The same asset object, unedited: no keystroke is needed to retrigger a
    // save, which was the only way to retry before.
    const result = await harness.lifecycle.retryDraft(draft);

    expect(result).toBe("saved");
    expect(harness.update).toHaveBeenLastCalledWith("asset_1", {
      notes: "The light turns about here",
      labels: [],
      removed: false,
      rotationDegrees: 0,
    });
    expect(harness.lifecycle.draftFor("asset_1")).toBeNull();
    expect(harness.lifecycle.saveStateFor("asset_1")).toBe("saved");
  });

  it("refuses a retry for a clip that has no draft", async () => {
    const harness = accepts();
    expect(await harness.lifecycle.retryDraft(asset())).toBe("refused");
    expect(harness.update).not.toHaveBeenCalled();
  });

  it("clears the mark when another write carries the draft to the backend", async () => {
    let refuse = true;
    const harness = lifecycleFor(async (assetId, patch) => {
      if (refuse) throw new Error("database is locked");
      return asset({ id: assetId, ...patch });
    });
    const draft = asset({ notes: "half a thought" });
    await harness.lifecycle.saveNotes(draft);
    refuse = false;

    // A label write sends the whole patch, draft included. Once the backend has
    // that text, saying it is unsaved would be a lie.
    await harness.lifecycle.submit(draft, { labels: ["arrival"] });

    expect(harness.lifecycle.draftFor("asset_1")).toBeNull();
  });
});

describe("a draft that outlives the read that contradicts it", () => {
  it("is written back over a freshly loaded queue", async () => {
    const harness = refuses();
    await harness.lifecycle.saveNotes(asset({ notes: "half a thought" }));

    // What a reload answers with: the catalog's value, which is not the draft.
    const loaded = [asset({ id: "asset_1", notes: "" }), asset({ id: "asset_2", notes: "other" })];
    const merged = harness.lifecycle.withDrafts(loaded);

    expect(merged.map((entry) => entry.notes)).toEqual(["half a thought", "other"]);
    // Untouched clips keep their identity, so nothing re-renders needlessly.
    expect(merged[1]).toBe(loaded[1]);
  });

  it("follows what the user types next rather than overwriting it", async () => {
    const harness = refuses();
    await harness.lifecycle.saveNotes(asset({ notes: "half a" }));

    harness.lifecycle.reviseDraft(asset({ notes: "half a thought" }));

    expect(harness.lifecycle.draftFor("asset_1")?.notes).toBe("half a thought");
    expect(harness.lifecycle.withDrafts([asset({ notes: "" })])[0].notes).toBe("half a thought");
  });

  it("leaves an untouched queue alone", () => {
    const harness = accepts();
    const loaded = [asset()];
    expect(harness.lifecycle.withDrafts(loaded)).toBe(loaded);
  });
});

describe("two submits at once", () => {
  it("refuses the identical second one instead of writing the same bytes twice", async () => {
    let release = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const harness = lifecycleFor(async (assetId, patch) => {
      await gate;
      return asset({ id: assetId, ...patch });
    });
    const draft = asset({ notes: "one save" });

    const first = harness.lifecycle.saveNotes(draft);
    // The double blur: the same text, while the first write is still in flight.
    const second = await harness.lifecycle.saveNotes(draft);

    expect(second).toBe("refused");
    release();
    expect(await first).toBe("saved");
    expect(harness.update).toHaveBeenCalledTimes(1);
  });

  it("lets a different value through, because each rotation is its own intention", async () => {
    let release = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const harness = lifecycleFor(async (assetId, patch) => {
      await gate;
      return asset({ id: assetId, ...patch });
    });
    const clip = asset();

    const first = harness.lifecycle.submit(clip, { rotationDegrees: 90 });
    const second = harness.lifecycle.submit(clip, { rotationDegrees: 180 });
    release();

    expect(await first).toBe("saved");
    expect(await second).toBe("saved");
    expect(harness.update).toHaveBeenCalledTimes(2);
  });
});
