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
    revision: 0,
    ...overrides,
  } as unknown as Asset;
}

interface Harness {
  lifecycle: AssetMutationLifecycle;
  applied: Asset[];
  errors: string[];
  snapshots: MutationSnapshot[];
  update: ReturnType<typeof vi.fn>;
  reread: ReturnType<typeof vi.fn>;
  /** What the last snapshot says. */
  state(): MutationSnapshot;
}

function lifecycleFor(
  update: (assetId: string, patch: AssetPatch) => Promise<Asset>,
  reread: (assetId: string) => Promise<Asset> = async (assetId) => asset({ id: assetId }),
): Harness {
  const applied: Asset[] = [];
  const errors: string[] = [];
  const snapshots: MutationSnapshot[] = [];
  const spy = vi.fn(update);
  const rereadSpy = vi.fn(reread);
  const lifecycle = new AssetMutationLifecycle({
    update: spy,
    reread: rereadSpy,
    applyAsset: (next) => applied.push(next),
    onError: (message) => errors.push(message),
    onChange: (snapshot) => snapshots.push(snapshot),
  });
  return {
    lifecycle,
    applied,
    errors,
    snapshots,
    update: spy,
    reread: rereadSpy,
    state: () => lifecycle.snapshot(),
  };
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
      // The revision the edit was decided from travels with it (issue #49).
      expectedRevision: 0,
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
      expectedRevision: 0,
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

  it("sends them one at a time, so the backend cannot answer them out of order", async () => {
    const releases: Array<() => void> = [];
    const harness = lifecycleFor(async (assetId, patch) => {
      await new Promise<void>((resolve) => releases.push(resolve));
      return asset({ id: assetId, ...patch });
    });
    const clip = asset();

    const first = harness.lifecycle.submit(clip, { rotationDegrees: 90 });
    const second = harness.lifecycle.submit(clip, { rotationDegrees: 180 });
    await Promise.resolve();

    // The second write has not been sent: there is nothing for the backend to
    // complete in the wrong order.
    expect(harness.update).toHaveBeenCalledTimes(1);

    releases[0]();
    expect(await first).toBe("saved");
    releases[1]();
    expect(await second).toBe("saved");

    expect(harness.update.mock.calls.map((call) => call[1].rotationDegrees)).toEqual([90, 180]);
  });

  it("leaves the user's latest intention in place when writes finish in reverse order", async () => {
    // The backend answers the *second* write first — the reverse completion the
    // old racing path allowed, which left the catalog holding 90 after the user
    // had asked for 180.
    const pending: Array<{ patch: AssetPatch; resolve: (value: Asset) => void }> = [];
    const harness = lifecycleFor(
      (assetId, patch) =>
        new Promise<Asset>((resolve) => {
          pending.push({ patch, resolve: (value) => resolve(value) });
        }),
    );
    const clip = asset();

    const first = harness.lifecycle.submit(clip, { rotationDegrees: 90 });
    const second = harness.lifecycle.submit(clip, { rotationDegrees: 180 });
    await Promise.resolve();

    // Only the first is out, so answering "second first" is not expressible:
    // the queue holds it until 90 has landed.
    expect(pending).toHaveLength(1);
    pending[0].resolve(asset({ id: "asset_1", ...pending[0].patch }));
    expect(await first).toBe("saved");

    await Promise.resolve();
    expect(pending).toHaveLength(2);
    pending[1].resolve(asset({ id: "asset_1", ...pending[1].patch }));
    expect(await second).toBe("saved");

    // Both the write that reached the backend last and the value left on screen
    // are the user's latest intention.
    expect(harness.update.mock.calls.at(-1)?.[1].rotationDegrees).toBe(180);
    expect(harness.applied.at(-1)?.rotationDegrees).toBe(180);
  });

  it("composes queued writes rather than letting a whole patch undo the one ahead of it", async () => {
    const releases: Array<() => void> = [];
    const harness = lifecycleFor(async (assetId, patch) => {
      await new Promise<void>((resolve) => releases.push(resolve));
      return asset({ id: assetId, ...patch });
    });
    const clip = asset();

    // Two different fields, submitted from the same stale asset object. The
    // second patch is built when it is sent, so it carries the first's label
    // instead of the empty list `clip` still shows.
    const first = harness.lifecycle.submit(clip, { labels: ["arrival"] });
    const second = harness.lifecycle.submit(clip, { rotationDegrees: 180 });
    await Promise.resolve();

    releases[0]();
    await first;
    await Promise.resolve();
    releases[1]();
    await second;

    expect(harness.update.mock.calls.at(-1)?.[1]).toEqual({
      notes: "",
      labels: ["arrival"],
      removed: false,
      rotationDegrees: 180,
      // Built from the first write's answer, so it also carries that write's
      // revision — which is exactly what stops it being refused as stale.
      expectedRevision: 0,
    });
    expect(harness.applied.at(-1)?.labels).toEqual(["arrival"]);
    expect(harness.applied.at(-1)?.rotationDegrees).toBe(180);
  });

  it("still runs writes to different clips at the same time", async () => {
    const releases: Array<() => void> = [];
    const harness = lifecycleFor(async (assetId, patch) => {
      await new Promise<void>((resolve) => releases.push(resolve));
      return asset({ id: assetId, ...patch });
    });

    const first = harness.lifecycle.submit(asset({ id: "asset_1" }), { rotationDegrees: 90 });
    const second = harness.lifecycle.submit(asset({ id: "asset_2" }), { rotationDegrees: 90 });
    await Promise.resolve();

    // Serialization is per clip: one slow write must not hold up another clip's.
    expect(harness.update).toHaveBeenCalledTimes(2);
    releases.forEach((release) => release());
    expect(await first).toBe("saved");
    expect(await second).toBe("saved");
  });

  it("keeps the queue moving after a write in it fails", async () => {
    let refuse = true;
    const releases: Array<() => void> = [];
    const harness = lifecycleFor(async (assetId, patch) => {
      await new Promise<void>((resolve) => releases.push(resolve));
      if (refuse) throw new Error("database is locked");
      return asset({ id: assetId, ...patch });
    });
    const clip = asset();

    const first = harness.lifecycle.submit(clip, { rotationDegrees: 90 });
    const second = harness.lifecycle.submit(clip, { rotationDegrees: 180 });
    await Promise.resolve();

    releases[0]();
    expect(await first).toBe("failed");
    refuse = false;
    await Promise.resolve();
    releases[1]();

    // The rejection rolled the first back; the write behind it is not stranded.
    expect(await second).toBe("saved");
    expect(harness.applied.at(-1)?.rotationDegrees).toBe(180);
  });
});


describe("a write the backend refuses as stale", () => {
  /** Somebody else writes once, between the read and the write. */
  function withAnotherWriter(other: Partial<Asset>, revision = 4) {
    const fresh = asset({ ...other, revision });
    let refusals = 0;
    const harness = lifecycleFor(
      async (assetId, patch) => {
        if (patch.expectedRevision !== fresh.revision) {
          refusals += 1;
          throw new Error(
            `stale asset revision: expected revision ${patch.expectedRevision}, catalog is at ${fresh.revision}`,
          );
        }
        return asset({ id: assetId, ...patch, revision: fresh.revision + 1 });
      },
      async () => fresh,
    );
    return { harness, fresh, refusals: () => refusals };
  }

  it("re-reads, re-sends the same edit, and saves it", async () => {
    const { harness } = withAnotherWriter({ labels: ["market"] });

    const result = await harness.lifecycle.saveNotes(asset({ notes: "the station clock" }));

    expect(result).toBe("saved");
    expect(harness.reread).toHaveBeenCalledWith("asset_1");
    // The retry carries the other writer's label and the new revision, so it
    // adds to their work instead of reversing it.
    expect(harness.update).toHaveBeenLastCalledWith("asset_1", {
      notes: "the station clock",
      labels: ["market"],
      removed: false,
      rotationDegrees: 0,
      expectedRevision: 4,
    });
  });

  it("keeps every field the user was not editing at the other writer's value", async () => {
    // The defect this whole change exists for: a stale whole patch carries the
    // old label list, so applying it would have *removed* the label rather
    // than merely missing it.
    const { harness } = withAnotherWriter({ labels: ["market"], rotationDegrees: 90 });

    await harness.lifecycle.saveNotes(asset({ notes: "the station clock" }));

    const sent = harness.update.mock.calls.at(-1)?.[1];
    expect(sent.labels).toEqual(["market"]);
    expect(sent.rotationDegrees).toBe(90);
  });

  it("publishes the other writer's value on the way through", async () => {
    const { harness } = withAnotherWriter({ labels: ["market"] });

    await harness.lifecycle.saveNotes(asset({ notes: "the station clock" }));

    // The queue shows what the catalog holds, before the retry lands as well
    // as after: the user finds out the label appeared.
    expect(harness.applied.some((applied) => applied.labels.includes("market"))).toBe(true);
  });

  it("keeps the draft when even the recovery fails", async () => {
    let revision = 4;
    const harness = lifecycleFor(
      async (_assetId, patch) => {
        revision += 1;
        throw new Error(`stale asset revision: expected ${patch.expectedRevision}, catalog is at ${revision}`);
      },
      async () => asset({ revision }),
    );

    const result = await harness.lifecycle.saveNotes(asset({ notes: "mine" }));

    expect(result).toBe("failed");
    // Human prose is not thrown away because two writers collided.
    expect(harness.lifecycle.draftFor("asset_1")?.notes).toBe("mine");
    expect(harness.lifecycle.saveStateFor("asset_1")).toBe("unsaved");
  });

  it("recovers once and no more, rather than busy-waiting against a third writer", async () => {
    let revision = 4;
    const harness = lifecycleFor(
      async (_assetId, patch) => {
        // Every read is immediately out of date: another writer is winning.
        revision += 1;
        throw new Error(`stale asset revision: expected ${patch.expectedRevision}, catalog is at ${revision}`);
      },
      async () => asset({ revision }),
    );

    await harness.lifecycle.saveNotes(asset({ notes: "mine" }));

    expect(harness.update).toHaveBeenCalledTimes(2);
    expect(harness.reread).toHaveBeenCalledTimes(1);
  });

  it("does not re-read for an ordinary failure", async () => {
    const harness = lifecycleFor(async () => {
      throw new Error("database is locked");
    });

    await harness.lifecycle.saveNotes(asset({ notes: "mine" }));

    expect(harness.reread).not.toHaveBeenCalled();
    expect(harness.errors.at(-1)).toBe("Error: database is locked");
  });
});
