/**
 * One owner for the asset write path.
 *
 * Every judgment the user makes — a note, a label, a removal, a rotation — went
 * through the same four steps written out by hand at each call site: apply
 * optimistically, send the whole `AssetPatch`, roll back on rejection, report
 * the error. The behaviour that mattered most was the least visible one: a
 * rejected note save kept the user's paragraph on screen only because the
 * rollback restored the asset object it had been handed, which already carried
 * the typed text. That is load-bearing, and it was an accident of argument
 * passing (#19).
 *
 * So the failure policy is now stated rather than implied:
 *
 *   rollback      — the optimistic change is undone. A label the backend
 *                   refused must not sit on screen looking accepted.
 *   retain-draft  — the optimistic change stays, and the asset is recorded as
 *                   carrying an unsaved draft with a retry the UI can offer.
 *                   Human prose is not something to throw away because SQLite
 *                   was busy (#29).
 *
 * The lifecycle is a plain class with injected effects so the state machine can
 * be tested without rendering anything; `useAssetMutations` is the thin React
 * binding.
 */

import { useMemo, useRef, useState } from "react";
import type { Asset, AssetPatch } from "../types";

export type SaveState = "idle" | "saving" | "saved" | "unsaved";

export type FailurePolicy = "rollback" | "retain-draft";

export type MutationResult = "saved" | "failed" | "refused";

/** A note the user wrote that the backend has not accepted. */
export interface NoteDraft {
  assetId: string;
  notes: string;
  error: string;
}

export interface MutationSnapshot {
  saveState: SaveState;
  /** The asset the current save state is about; nothing else should display it. */
  activeAssetId: string | null;
  drafts: Record<string, NoteDraft>;
}

export interface AssetMutationEffects {
  /** The IPC write. Given the whole patch, as the `update_asset` contract requires. */
  update(assetId: string, patch: AssetPatch): Promise<Asset>;
  /** Put an asset into the queue — optimistic value, backend value, or rollback. */
  applyAsset(asset: Asset): void;
  /** Where a rejection is reported to the user. */
  onError(message: string): void;
  /** Snapshot changed. */
  onChange(snapshot: MutationSnapshot): void;
}

export function patchFor(asset: Asset): AssetPatch {
  return {
    notes: asset.notes,
    labels: asset.labels,
    removed: asset.removed,
    rotationDegrees: asset.rotationDegrees,
  };
}

const EMPTY_SNAPSHOT: MutationSnapshot = { saveState: "idle", activeAssetId: null, drafts: {} };

export class AssetMutationLifecycle {
  private readonly effects: AssetMutationEffects;
  private snapshotValue: MutationSnapshot = EMPTY_SNAPSHOT;
  /**
   * assetId → the payload of the write outstanding for it, in flight or waiting
   * its turn. What the double-submit guard compares against.
   */
  private readonly outstanding = new Map<string, string>();
  /**
   * assetId → the tail of that asset's write chain.
   *
   * `update_asset` takes a whole `AssetPatch` and the transport carries no
   * revision token, so the backend cannot reconcile two overlapping writes: the
   * one that happens to land second wins, whichever the user asked for first.
   * Two rotations completing in reverse order left the catalog — and the queue,
   * since the reply is applied — showing the older angle. So writes to one asset
   * are run one after another; writes to different assets stay concurrent.
   */
  private readonly chain = new Map<string, Promise<void>>();
  /**
   * assetId → the newest value known for it while writes are outstanding,
   * optimistic or backend-confirmed. A queued write builds its patch from this
   * at send time, so it carries what the write ahead of it changed instead of
   * the values those fields held when the user asked for this one.
   */
  private readonly latest = new Map<string, Asset>();

  constructor(effects: AssetMutationEffects) {
    this.effects = effects;
  }

  snapshot(): MutationSnapshot {
    return this.snapshotValue;
  }

  draftFor(assetId: string): NoteDraft | null {
    return this.snapshotValue.drafts[assetId] ?? null;
  }

  /** The save state to show beside a clip: another clip's save is not this clip's news. */
  saveStateFor(assetId: string): SaveState {
    if (this.snapshotValue.drafts[assetId]) return "unsaved";
    return this.snapshotValue.activeAssetId === assetId ? this.snapshotValue.saveState : "idle";
  }

  /**
   * Write `changes` to an asset.
   *
   * Refuses a second identical submit while the first is outstanding: that is
   * the double-submit case (a blur that fires twice, a keyboard save landing on
   * the same text), and it is refused rather than queued because sending the
   * same bytes twice is never what the user meant. A different value is a new
   * intention and is kept — holding one would break rotation, where each press
   * is a distinct write — but it is queued behind the write already going out
   * rather than raced against it, so the last intention is the one that lands.
   */
  async submit(
    asset: Asset,
    changes: Partial<AssetPatch>,
    policy: FailurePolicy = "rollback",
  ): Promise<MutationResult> {
    // While writes are outstanding the newest value is the one they produced;
    // with nothing outstanding it is the caller's, which a reload may have
    // refreshed underneath us.
    const base = this.latest.get(asset.id) ?? asset;
    const payload = JSON.stringify({ ...patchFor(base), ...changes });
    if (this.outstanding.get(asset.id) === payload) return "refused";

    this.outstanding.set(asset.id, payload);
    this.latest.set(asset.id, base);

    const queued = (this.chain.get(asset.id) ?? Promise.resolve()).then(() =>
      this.write(asset.id, changes, policy),
    );
    // The tail must settle whatever happened, or one rejection would strand
    // every write queued behind it. `write` already resolves on failure; this
    // covers the enqueue itself.
    const tail = queued.then(() => undefined, () => undefined);
    this.chain.set(asset.id, tail);

    try {
      return await queued;
    } finally {
      if (this.outstanding.get(asset.id) === payload) this.outstanding.delete(asset.id);
      // Last one out forgets the asset, so the next submit starts from what the
      // caller is rendering rather than from a value a reload has superseded.
      if (this.chain.get(asset.id) === tail) {
        this.chain.delete(asset.id);
        this.latest.delete(asset.id);
      }
    }
  }

  /**
   * Send one write, once its turn comes.
   *
   * The patch is built here rather than at `submit`: a write that waited behind
   * another must carry that one's result in the fields it is not itself
   * changing, or the whole-patch contract would silently undo it.
   */
  private async write(
    assetId: string,
    changes: Partial<AssetPatch>,
    policy: FailurePolicy,
  ): Promise<MutationResult> {
    const base = this.latest.get(assetId);
    // Only reachable if the chain outlived its own bookkeeping; nothing to write.
    if (!base) return "refused";

    const optimistic = { ...base, ...changes };
    this.apply(optimistic);
    this.update({ saveState: "saving", activeAssetId: assetId });

    try {
      const updated = await this.effects.update(assetId, { ...patchFor(base), ...changes });
      this.apply(updated);
      // A write carrying the retained draft is the draft being saved, whatever
      // else prompted it: the backend now holds that text, so the unsaved mark
      // would be a lie.
      const drafts = this.clearDraftIfWritten(assetId, updated);
      this.update({ saveState: "saved", activeAssetId: assetId, drafts });
      return "saved";
    } catch (reason) {
      const message = String(reason);
      if (policy === "retain-draft") {
        // The draft stays on screen and stays marked, so the user can send the
        // same text again without retyping it or touching the field to
        // retrigger a blur.
        this.apply(optimistic);
        this.update({
          saveState: "unsaved",
          activeAssetId: assetId,
          drafts: {
            ...this.snapshotValue.drafts,
            [assetId]: { assetId, notes: optimistic.notes, error: message },
          },
        });
      } else {
        this.apply(base);
        this.update({ saveState: "idle", activeAssetId: assetId });
      }
      this.effects.onError(message);
      return "failed";
    }
  }

  /** Publish an asset value and remember it as the base for anything queued behind. */
  private apply(asset: Asset) {
    if (this.latest.has(asset.id)) this.latest.set(asset.id, asset);
    this.effects.applyAsset(asset);
  }

  /** Save the notes currently on the asset, keeping them on screen if the write fails. */
  saveNotes(asset: Asset, notes = asset.notes): Promise<MutationResult> {
    return this.submit(asset, { notes }, "retain-draft");
  }

  /** Send the retained draft again, unchanged. */
  retryDraft(asset: Asset): Promise<MutationResult> {
    const draft = this.snapshotValue.drafts[asset.id];
    if (!draft) return Promise.resolve("refused");
    return this.submit(asset, { notes: draft.notes }, "retain-draft");
  }

  /**
   * Keep a retained draft in step with what the user is typing. The draft is
   * the unsaved text, so an edit to a clip that already carries one revises it;
   * otherwise the next read would put the failed draft back over the new
   * keystrokes.
   */
  reviseDraft(asset: Asset) {
    const draft = this.snapshotValue.drafts[asset.id];
    if (!draft || draft.notes === asset.notes) return;
    this.update({
      drafts: { ...this.snapshotValue.drafts, [asset.id]: { ...draft, notes: asset.notes } },
    });
  }

  /** Forget a draft without writing it — the user replaced or abandoned it. */
  discardDraft(assetId: string) {
    if (!this.snapshotValue.drafts[assetId]) return;
    const drafts = { ...this.snapshotValue.drafts };
    delete drafts[assetId];
    this.update({ drafts, saveState: "idle", activeAssetId: assetId });
  }

  /**
   * The assets a caller should show, with retained drafts written back over
   * them. A reload answers with what the backend holds, which is precisely not
   * the draft; without this an unsaved paragraph would vanish on the next read.
   */
  withDrafts(assets: Asset[]): Asset[] {
    const drafts = this.snapshotValue.drafts;
    if (Object.keys(drafts).length === 0) return assets;
    let changed = false;
    const merged = assets.map((asset) => {
      const draft = drafts[asset.id];
      if (!draft || draft.notes === asset.notes) return asset;
      changed = true;
      return { ...asset, notes: draft.notes };
    });
    return changed ? merged : assets;
  }

  private clearDraftIfWritten(assetId: string, written: Asset): Record<string, NoteDraft> {
    const draft = this.snapshotValue.drafts[assetId];
    if (!draft || draft.notes !== written.notes) return this.snapshotValue.drafts;
    const drafts = { ...this.snapshotValue.drafts };
    delete drafts[assetId];
    return drafts;
  }

  private update(patch: Partial<MutationSnapshot>) {
    this.snapshotValue = { ...this.snapshotValue, ...patch };
    this.effects.onChange(this.snapshotValue);
  }
}

export interface UseAssetMutations extends MutationSnapshot {
  lifecycle: AssetMutationLifecycle;
  submit(asset: Asset, changes: Partial<AssetPatch>): Promise<MutationResult>;
  saveNotes(asset: Asset, notes?: string): Promise<MutationResult>;
  retryDraft(asset: Asset): Promise<MutationResult>;
  discardDraft(assetId: string): void;
  saveStateFor(assetId: string): SaveState;
  withDrafts(assets: Asset[]): Asset[];
}

export function useAssetMutations(
  effects: Omit<AssetMutationEffects, "onChange">,
): UseAssetMutations {
  const [snapshot, setSnapshot] = useState<MutationSnapshot>(EMPTY_SNAPSHOT);
  // The effects are read through a ref so the lifecycle survives every render
  // while still calling this render's callbacks.
  const effectsRef = useRef(effects);
  effectsRef.current = effects;

  const lifecycle = useMemo(
    () =>
      new AssetMutationLifecycle({
        update: (assetId, patch) => effectsRef.current.update(assetId, patch),
        applyAsset: (asset) => effectsRef.current.applyAsset(asset),
        onError: (message) => effectsRef.current.onError(message),
        onChange: setSnapshot,
      }),
    [],
  );

  return {
    ...snapshot,
    lifecycle,
    submit: (asset, changes) => lifecycle.submit(asset, changes),
    saveNotes: (asset, notes) => lifecycle.saveNotes(asset, notes),
    retryDraft: (asset) => lifecycle.retryDraft(asset),
    discardDraft: (assetId) => lifecycle.discardDraft(assetId),
    saveStateFor: (assetId) => lifecycle.saveStateFor(assetId),
    withDrafts: (assets) => lifecycle.withDrafts(assets),
  };
}
