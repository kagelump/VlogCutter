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
  /** assetId → the payload of the write currently in flight for it. */
  private readonly inFlight = new Map<string, string>();

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
   * Refuses a second identical submit while the first is still in flight: that
   * is the double-submit case (a blur that fires twice, a keyboard save landing
   * on the same text), and it is refused rather than queued because sending the
   * same bytes twice is never what the user meant. A different value for the
   * same field is a new intention and goes through — holding one would break
   * rotation, where each press is a distinct write.
   */
  async submit(
    asset: Asset,
    changes: Partial<AssetPatch>,
    policy: FailurePolicy = "rollback",
  ): Promise<MutationResult> {
    const payload = JSON.stringify({ ...patchFor(asset), ...changes });
    if (this.inFlight.get(asset.id) === payload) return "refused";
    this.inFlight.set(asset.id, payload);

    const optimistic = { ...asset, ...changes };
    this.effects.applyAsset(optimistic);
    this.update({ saveState: "saving", activeAssetId: asset.id });

    try {
      const updated = await this.effects.update(asset.id, { ...patchFor(asset), ...changes });
      this.effects.applyAsset(updated);
      // A write carrying the retained draft is the draft being saved, whatever
      // else prompted it: the backend now holds that text, so the unsaved mark
      // would be a lie.
      const drafts = this.clearDraftIfWritten(asset.id, updated);
      this.update({ saveState: "saved", activeAssetId: asset.id, drafts });
      return "saved";
    } catch (reason) {
      const message = String(reason);
      if (policy === "retain-draft") {
        // The draft stays on screen and stays marked, so the user can send the
        // same text again without retyping it or touching the field to
        // retrigger a blur.
        this.effects.applyAsset(optimistic);
        this.update({
          saveState: "unsaved",
          activeAssetId: asset.id,
          drafts: {
            ...this.snapshotValue.drafts,
            [asset.id]: { assetId: asset.id, notes: optimistic.notes, error: message },
          },
        });
      } else {
        this.effects.applyAsset(asset);
        this.update({ saveState: "idle", activeAssetId: asset.id });
      }
      this.effects.onError(message);
      return "failed";
    } finally {
      if (this.inFlight.get(asset.id) === payload) this.inFlight.delete(asset.id);
    }
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
