/**
 * Deterministic fixture builders.
 *
 * Ids, filenames, paths, sizes, and times are functions of the scenario, never
 * of the run. `asset_sony_a7c_ii_001` at `2026-02-08T00:14:08.000Z` means the
 * same thing in every run and in every failure message; a `randomUUID` would
 * make a diff unreadable and a snapshot worthless.
 */

import type {
  ArchiveRoot,
  Asset,
  CardCandidate,
  ClipTranscript,
  AssetAnalysisView,
  Project,
  SyncStatus,
} from "../../types";

export function slugify(value: string): string {
  return value
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_|_$/g, "");
}

/** `Sony A7C II` becomes `SONY`, the way a camera writes its own files. */
export function filenamePrefix(camera: string): string {
  const first = camera.trim().split(/\s+/)[0] ?? "CAM";
  return first.toUpperCase().replace(/[^A-Z0-9]/g, "");
}

export function counterSuffix(index: number): string {
  return String(index + 1).padStart(3, "0");
}

export interface ProjectSpec {
  id?: string;
  name: string;
  createdAt?: string;
}

export function buildProject(spec: ProjectSpec): Project {
  return {
    id: spec.id ?? `project_${slugify(spec.name)}`,
    name: spec.name,
    createdAt: spec.createdAt ?? "2026-08-01T09:00:00.000Z",
    assetCount: 0,
    labeledCount: 0,
    removedCount: 0,
    originNodeId: null,
    pairedServerNodeId: null,
    pairedAt: null,
  };
}

export interface CameraSpec {
  /** How many clips this camera contributes. */
  clips: number;
  /** First clip's capture time. Later clips follow at `spacingSeconds`. */
  startAt?: string;
  spacingSeconds?: number;
  durationSeconds?: number;
  /** Whether each clip has a review proxy. A number means "the first N do". */
  withProxy?: boolean | number;
  originalAvailable?: boolean;
  volume?: string;
  extension?: string;
  width?: number;
  height?: number;
  frameRate?: number;
  codec?: string;
  /** Rotation the camera recorded in metadata, before any user correction. */
  metadataRotationDegrees?: number;
  orientationProbed?: boolean;
}

export interface BuildAssetsOptions {
  projectId: string;
  camera: string;
  spec: CameraSpec;
}

export function buildCameraAssets({ projectId, camera, spec }: BuildAssetsOptions): Asset[] {
  const slug = slugify(camera);
  const prefix = filenamePrefix(camera);
  const extension = spec.extension ?? "MP4";
  const volume = spec.volume ?? "/Volumes/Travel";
  const start = Date.parse(spec.startAt ?? "2026-02-08T00:00:00.000Z");
  const spacing = (spec.spacingSeconds ?? 300) * 1000;
  const duration = spec.durationSeconds ?? 18.4;
  const proxyCount =
    spec.withProxy === true ? spec.clips : spec.withProxy === false || spec.withProxy === undefined ? 0 : spec.withProxy;

  return Array.from({ length: spec.clips }, (_, index) => {
    const suffix = counterSuffix(index);
    const id = `asset_${slug}_${suffix}`;
    const filename = `${prefix}_${suffix}.${extension}`;
    const capturedAt = new Date(start + index * spacing).toISOString();
    const hasProxy = index < proxyCount;
    const originalAvailable = spec.originalAvailable ?? true;
    return {
      id,
      projectId,
      path: `${volume}/${slug}/${filename}`,
      filename,
      fingerprint: `fp_${slug}_${suffix}`,
      sizeBytes: 400_000_000 + index * 1_000_000,
      durationSeconds: duration,
      frameRate: spec.frameRate ?? 59.94,
      width: spec.width ?? 3840,
      height: spec.height ?? 2160,
      codec: spec.codec ?? "hevc",
      camera,
      capturedAtRaw: capturedAt,
      bestTime: capturedAt,
      offsetSeconds: 0,
      proxyPath: hasProxy ? `/Users/harness/Movies/proxies/${id}.mp4` : null,
      proxyRepresentation: hasProxy ? "metadata_baked_v1" : null,
      metadataRotationDegrees: spec.metadataRotationDegrees ?? 0,
      defaultRotationDegrees: spec.metadataRotationDegrees ?? 0,
      rotationDegrees: spec.metadataRotationDegrees ?? 0,
      // Probed by default: an unprobed clip makes the stage show its "reading
      // orientation" slate and sends the app off to probe, which is a state a
      // scenario should ask for rather than inherit.
      orientationProbed: spec.orientationProbed ?? true,
      originalAvailable,
      proxyAvailable: hasProxy,
      thumbnailPath: null,
      thumbnailQuality: 0,
      notes: "",
      title: "",
      description: "",
      labels: [],
      removed: false,
      mediaObjectId: null,
      mediaVerificationState: "verified",
      available: originalAvailable || hasProxy,
      phase: null,
      location: null,
      timezone: null,
      storyBeat: null,
    } satisfies Asset;
  });
}

export interface TranscriptSpec {
  status?: ClipTranscript["status"];
  language?: string;
  text?: string;
  speechSeconds?: number;
}

export function buildTranscript(assetId: string, spec: TranscriptSpec = {}): ClipTranscript {
  const status = spec.status ?? (spec.text ? "speech" : "no_speech");
  const text = spec.text ?? "";
  return {
    assetId,
    recipeKey: "stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1",
    sourceFingerprint: `proxy_${assetId}`,
    status,
    rejectReason: "",
    primaryLanguage: spec.language ?? (text ? "en" : ""),
    speechSeconds: spec.speechSeconds ?? (text ? 6.5 : 0),
    windowCount: text ? 1 : 0,
    segments: text
      ? [
          {
            startMs: 1_200,
            endMs: 6_400,
            language: spec.language ?? "en",
            text,
            meanTokenProbability: 0.82,
          },
        ]
      : [],
  };
}

export interface AnalysisSpec {
  usable?: boolean;
  unusableReason?: AssetAnalysisView["analysis"]["unusableReason"];
  aiLabels?: string[];
  cameraMovement?: AssetAnalysisView["analysis"]["cameraMovement"];
  ocrText?: string[];
  transcriptUsed?: boolean;
  costUsd?: number;
  createdAt?: string;
  current?: boolean;
}

export function buildAnalysis(assetId: string, spec: AnalysisSpec = {}): AssetAnalysisView {
  const usable = spec.usable ?? true;
  return {
    analysis: {
      assetId,
      recipeKey: "framepack.2fps.skip400ms.max40.win3-8.sharp-bright-motion.dhash8-d5.v1",
      promptVersion: "vlm.observe.v2",
      provider: "openrouter",
      model: "qwen/qwen3-vl-flash",
      status: "ok",
      rejectReason: "",
      usable,
      unusableReason: usable ? null : spec.unusableReason ?? "blurry",
      bestMomentStartMs: usable ? 4_200 : null,
      bestMomentEndMs: usable ? 9_800 : null,
      thumbnailTimestampMs: 6_100,
      cameraMovement: spec.cameraMovement ?? "static",
      ocrText: spec.ocrText ?? [],
      transcriptUsed: spec.transcriptUsed ?? false,
      promptTokens: 2_411,
      completionTokens: 188,
      costUsd: spec.costUsd ?? 0.0001084,
      latencyMs: 3_820,
      attempts: 1,
      createdAt: spec.createdAt ?? "2026-08-03T11:04:12.000Z",
    },
    aiLabels: spec.aiLabels ?? [],
    current: spec.current ?? true,
  };
}

export interface ArchiveRootSpec {
  id?: string;
  localMountPath?: string;
  available?: boolean;
  availableBytes?: number | null;
  createdAt?: string;
}

export function buildArchiveRoot(displayName: string, spec: ArchiveRootSpec = {}): ArchiveRoot {
  return {
    id: spec.id ?? `archive_${slugify(displayName)}`,
    displayName,
    localMountPath: spec.localMountPath ?? `/Volumes/${displayName.replace(/\s+/g, "")}`,
    available: spec.available ?? true,
    availableBytes: spec.availableBytes ?? 1_200_000_000_000,
    createdAt: spec.createdAt ?? "2026-08-01T09:00:00.000Z",
  };
}

export interface CardSpec {
  sessionId?: string;
  mountPath?: string;
  cameraHint?: string;
  clipCount?: number;
  totalBytes?: number;
}

export function buildCard(label: string, spec: CardSpec = {}): CardCandidate {
  return {
    sessionId: spec.sessionId ?? `card_${slugify(label)}`,
    label,
    mountPath: spec.mountPath ?? `/Volumes/${label}`,
    cameraHint: spec.cameraHint ?? "",
    clipCount: spec.clipCount ?? 0,
    totalBytes: spec.totalBytes ?? 0,
  };
}

export function buildSyncStatus(projectId: string, overrides: Partial<SyncStatus> = {}): SyncStatus {
  return {
    projectId,
    state: "unpaired",
    lastSyncAt: null,
    lastError: null,
    localPendingOperations: 0,
    remotePendingOperations: 0,
    conflictCount: 0,
    conflictDetails: [],
    activeEndpoint: null,
    backoffUntil: null,
    ...overrides,
  };
}
