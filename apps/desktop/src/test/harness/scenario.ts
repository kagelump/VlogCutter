/**
 * The scenario builder.
 *
 * A CUJ's setup should read like the situation a user is in, not like DTO
 * construction. Lower-level state stays reachable through `harness.state` for
 * the cases a fixture cannot express.
 */

import { HarnessClock } from "./clock";
import { DesktopHarness } from "./DesktopHarness";
import {
  buildAnalysis,
  buildArchiveRoot,
  buildCameraAssets,
  buildCard,
  buildProject,
  buildTranscript,
  buildSyncStatus,
  type AnalysisSpec,
  type ArchiveRootSpec,
  type CameraSpec,
  type CardSpec,
  type TranscriptSpec,
} from "./builders";
import { createInitialState, putAsset, type DesktopHarnessState } from "./state";
import { buildChronologyOverview, type ChronologySpec } from "./chronologyFixtures";
import type {
  ChronologyOverview,
  Contradiction,
  Diagnostic,
  OtioExportResult,
  ValidationReport,
} from "../../chronologyTypes";
import type { Asset, DesktopSettings, SyncStatus } from "../../types";

export interface ScenarioOptions {
  /** The instant the scenario starts at. Every fake timestamp derives from it. */
  now?: string;
}

export class ScenarioBuilder {
  private readonly clock: HarnessClock;
  private readonly state: DesktopHarnessState;
  private currentProjectId = "";
  private lastEditSequenceId = "";

  constructor(options: ScenarioOptions = {}) {
    this.clock = new HarnessClock(options.now);
    this.state = createInitialState(this.clock);
  }

  /** Open a project. The first one declared is the one the app lands in. */
  project(name: string, options: { id?: string; createdAt?: string } = {}): this {
    const project = buildProject({ name, ...options });
    this.state.projects.push(project);
    this.currentProjectId = project.id;
    return this;
  }

  /** Add a camera's clips to the current project. */
  camera(name: string, spec: CameraSpec): this {
    const projectId = this.requireProject();
    for (const asset of buildCameraAssets({ projectId, camera: name, spec })) {
      putAsset(this.state, asset);
    }
    return this;
  }

  /** Add one asset built by hand, for a case the camera spec cannot express. */
  asset(asset: Asset): this {
    putAsset(this.state, asset);
    return this;
  }

  withProxy(...assetIds: string[]): this {
    for (const assetId of assetIds) {
      const asset = this.requireAsset(assetId);
      putAsset(this.state, {
        ...asset,
        proxyAvailable: true,
        proxyPath: `/Users/harness/Movies/proxies/${assetId}.v3.mp4`,
        proxyRepresentation: "raw_v3",
      });
    }
    return this;
  }

  withoutProxy(...assetIds: string[]): this {
    for (const assetId of assetIds) {
      const asset = this.requireAsset(assetId);
      putAsset(this.state, {
        ...asset,
        proxyAvailable: false,
        proxyPath: null,
        proxyRepresentation: null,
      });
    }
    return this;
  }

  offlineOriginal(...assetIds: string[]): this {
    for (const assetId of assetIds) {
      const asset = this.requireAsset(assetId);
      // `putAsset` re-derives `available`: with a proxy the clip stays
      // reviewable, without one it goes dark.
      putAsset(this.state, { ...asset, originalAvailable: false });
    }
    return this;
  }

  /**
   * A proxy made by an older recipe. It plays, but the app counts it as work
   * outstanding and regenerates it — the same as the backend's
   * `has_current_proxy`.
   */
  withStaleProxy(...assetIds: string[]): this {
    for (const assetId of assetIds) {
      const asset = this.requireAsset(assetId);
      putAsset(this.state, {
        ...asset,
        proxyAvailable: true,
        proxyPath: `/Users/harness/Movies/proxies/${assetId}.mp4`,
        proxyRepresentation: "metadata_baked_v1",
      });
    }
    return this;
  }

  /** The loopback media origin failed to bind, so nothing has a URL to play. */
  withoutMediaOrigin(): this {
    this.state.mediaOrigin = null;
    return this;
  }

  /** Clips a relink at this folder would actually find. */
  relinkableAt(path: string, ...assetIds: string[]): this {
    this.state.relinkable[path] = [...(this.state.relinkable[path] ?? []), ...assetIds];
    return this;
  }

  withTranscript(assetId: string, spec: TranscriptSpec = {}): this {
    this.requireAsset(assetId);
    this.state.transcripts[assetId] = buildTranscript(assetId, spec);
    return this;
  }

  withAnalysis(assetId: string, spec: AnalysisSpec = {}): this {
    this.requireAsset(assetId);
    this.state.analyses[assetId] = buildAnalysis(assetId, spec);
    return this;
  }

  withNote(assetId: string, notes: string): this {
    const asset = this.requireAsset(assetId);
    putAsset(this.state, { ...asset, notes });
    return this;
  }

  withLabels(assetId: string, ...labels: string[]): this {
    const asset = this.requireAsset(assetId);
    putAsset(this.state, { ...asset, labels: [...labels] });
    return this;
  }

  withSummary(assetId: string, title: string, description = ""): this {
    const asset = this.requireAsset(assetId);
    putAsset(this.state, { ...asset, title, description });
    return this;
  }

  removed(...assetIds: string[]): this {
    for (const assetId of assetIds) {
      const asset = this.requireAsset(assetId);
      putAsset(this.state, { ...asset, removed: true });
    }
    return this;
  }

  /** What the desktop recorded as the user's place when it last closed. */
  selectedClip(assetId: string): this {
    this.requireAsset(assetId);
    this.state.settings.lastProjectId = this.currentProjectId;
    this.state.settings.lastAssetId = assetId;
    this.state.settings.lastContextUpdatedAt = this.clock.iso();
    return this;
  }

  settings(patch: Partial<DesktopSettings>): this {
    Object.assign(this.state.settings, patch);
    return this;
  }

  missingPrerequisite(name: string): this {
    this.state.prerequisites = this.state.prerequisites.map((tool) =>
      tool.name === name ? { ...tool, available: false } : tool,
    );
    return this;
  }

  modelsInstalled(installed: boolean): this {
    this.state.models = this.state.models.map((model) => ({ ...model, installed }));
    return this;
  }

  apiKeyPresent(present: boolean): this {
    this.state.openRouterKeyPresent = present;
    this.state.openRouterKey = present ? "harness-key" : "";
    return this;
  }

  /**
   * A declared chronology outcome. Nothing here computes an order: the spec
   * states the order the solver produced, and the fixture turns it into the DTO
   * the backend would return.
   */
  chronology(spec: ChronologySpec | ChronologyOverview): this {
    this.state.chronology.overview = "order" in spec ? buildChronologyOverview(spec) : spec;
    this.lastEditSequenceId = "";
    return this;
  }

  /**
   * "When the user corrects this sequence's clock…" — names an edit so the
   * scenario can declare what the backend returns afterwards. Pair with
   * `thenChronology`.
   */
  whenClockEpochApplied(sequenceId: string): this {
    this.lastEditSequenceId = sequenceId;
    return this;
  }

  /** …"the backend then returns this ordering." */
  thenChronology(spec: ChronologySpec | ChronologyOverview): this {
    if (!this.lastEditSequenceId) {
      throw new Error("Call whenClockEpochApplied(sequenceId) before thenChronology(...)");
    }
    this.state.chronology.outcomesByEdit[this.lastEditSequenceId] =
      "order" in spec ? buildChronologyOverview(spec) : spec;
    return this;
  }

  /** What previewing an edit to this sequence reports back. */
  contradictionOnPreview(sequenceId: string, ...contradictions: Contradiction[]): this {
    this.state.chronology.previewsByEdit[sequenceId] = contradictions;
    return this;
  }

  /** Where the backend resumes the queue after a reorder, when not the anchor. */
  resumesAt(assetId: string): this {
    this.state.chronology.restoreTo = assetId;
    return this;
  }

  diagnostic(diagnostic: Diagnostic): this {
    this.state.diagnostics.push(diagnostic);
    return this;
  }

  /** Findings validation returns for a revision. An undeclared one is clean. */
  editPlanValidation(revisionId: string, report: ValidationReport): this {
    this.state.editPlans.validation[revisionId] = report;
    return this;
  }

  /** The next edit-plan write is refused as stale, as a concurrent change would. */
  editPlanConflict(message: string): this {
    this.state.editPlans.conflictMessage = message;
    return this;
  }

  /** An OTIO file already on disk at this path, with this digest. */
  existingExport(result: OtioExportResult): this {
    this.state.editPlans.exportResult = result;
    return this;
  }

  /**
   * A card in the slot, and the clips that are on it. `newClips` of them are
   * not yet in the project; the rest are already known, which is what makes the
   * second-insertion case testable.
   */
  cardInserted(label: string, spec: CardSpec & { clips?: number; newClips?: number; camera?: string } = {}): this {
    const projectId = this.requireProject();
    const card = buildCard(label, { clipCount: spec.clips, ...spec });
    this.state.cards.push(card);

    const total = spec.clips ?? 0;
    if (total === 0) return this;
    const camera = spec.camera ?? spec.cameraHint ?? label;
    const onCard = buildCameraAssets({
      projectId,
      camera,
      spec: { clips: total, startAt: "2026-02-09T08:00:00.000Z", volume: card.mountPath },
    });
    const newClips = spec.newClips ?? total;
    // The clips the project already has are in the catalog; the additions are
    // only on the card until an import brings them over.
    for (const asset of onCard.slice(0, total - newClips)) putAsset(this.state, asset);
    this.state.imports.additions[card.mountPath] = onCard;
    return this;
  }

  /** More footage on a card the project has already seen once. */
  cardGainedClips(mountPath: string, clips: number): this {
    const projectId = this.requireProject();
    const existing = this.state.imports.additions[mountPath] ?? [];
    const card = this.state.cards.find((candidate) => candidate.mountPath === mountPath);
    if (!card) throw new Error(`No card is mounted at ${mountPath}`);
    const camera = existing[0]?.camera ?? card.label;
    const grown = buildCameraAssets({
      projectId,
      camera,
      spec: { clips: existing.length + clips, startAt: "2026-02-09T08:00:00.000Z", volume: mountPath },
    });
    this.state.imports.additions[mountPath] = grown;
    return this;
  }

  archiveRoot(displayName: string, spec: ArchiveRootSpec = {}): this {
    this.state.archiveRoots.push(buildArchiveRoot(displayName, spec));
    return this;
  }

  paired(options: { endpoint?: string; label?: string; nodeId?: string } = {}): this {
    this.state.settings.pairedServerNodeId = options.nodeId ?? "node_server_harness";
    this.state.settings.pairedServerEndpoint = options.endpoint ?? "https://rcdvlog.local";
    this.state.settings.pairedServerLabel = options.label ?? "Studio server";
    this.state.settings.pairedAt = this.clock.iso();
    this.state.settings.pairedServerEndpoints = [
      { url: options.endpoint ?? "https://rcdvlog.local", kind: "lan", label: options.label ?? "Studio server" },
    ];
    return this;
  }

  syncStatus(overrides: Partial<SyncStatus>): this {
    const projectId = this.requireProject();
    this.state.sync[projectId] = buildSyncStatus(projectId, overrides);
    return this;
  }

  /** Native dialog answers, consumed in order. */
  dialogAnswers(...paths: Array<string | null>): this {
    this.state.dialogResponses.push(...paths);
    return this;
  }

  /** Escape hatch for state a fixture method does not cover. */
  tap(edit: (state: DesktopHarnessState) => void): this {
    edit(this.state);
    return this;
  }

  build(): DesktopHarness {
    return new DesktopHarness(this.state, this.clock);
  }

  private requireProject(): string {
    if (!this.currentProjectId) throw new Error("Declare a project before adding clips to it");
    return this.currentProjectId;
  }

  private requireAsset(assetId: string): Asset {
    const asset = this.state.assets[assetId];
    if (!asset) {
      throw new Error(
        `Unknown asset in scenario: ${assetId}. Declared: ${this.state.assetOrder.join(", ") || "(none)"}`,
      );
    }
    return asset;
  }
}

export function desktopScenario(options: ScenarioOptions = {}): ScenarioBuilder {
  return new ScenarioBuilder(options);
}
