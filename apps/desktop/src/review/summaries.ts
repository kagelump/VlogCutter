import type { Asset, AssetAnalysisView, ClipTranscript, TranscriptStatus } from "../types";

/**
 * One-line summaries for collapsed surfaces (spec §3.6).
 *
 * A collapsed section carries a summary of its own content, so the
 * information is present at a glance and expanding it is for editing rather
 * than for discovery.
 */

export const transcriptStatusLabels: Record<TranscriptStatus, string> = {
  speech: "Speech",
  // Silence is a result, not a failure: roughly half of a real corpus is silent.
  no_speech: "No speech detected",
  no_audio: "No audio stream",
  no_accepted_speech: "Speech detected, no accepted transcript",
  rejected: "Discarded as unreliable",
  failed: "Transcription failed",
  review_required: "Review required — quality evidence incomplete",
};

export function formatBytes(bytes: number) {
  if (bytes < 1_000_000_000) return `${(bytes / 1_000_000).toFixed(0)} MB`;
  if (bytes >= 1_000_000_000_000) return `${(bytes / 1_000_000_000_000).toFixed(1)} TB`;
  return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
}

export function transcriptSummary(transcript: ClipTranscript | null): string {
  if (!transcript) return "Not transcribed";

  const label = transcriptStatusLabels[transcript.status];
  if (transcript.status === "review_required") {
    const count = transcript.segments.length;
    return `${label} · ${count} candidate segment${count === 1 ? "" : "s"} retained, not accepted`;
  }
  if (transcript.status !== "speech") {
    const reason = transcript.rejectReason.trim();
    return reason ? `${label} · ${reason}` : label;
  }

  const count = transcript.segments.length;
  const parts = [label, `${count} segment${count === 1 ? "" : "s"}`];
  const language = transcript.primaryLanguage.trim();
  if (language) parts.push(language);
  return parts.join(" · ");
}

/** The tab badge: how many segments there are, or nothing to show. */
export function transcriptBadge(transcript: ClipTranscript | null): number | null {
  if (!transcript || transcript.segments.length === 0) return null;
  return transcript.segments.length;
}

export function analysisSummary(observation: AssetAnalysisView | null): string {
  if (!observation) return "Not analyzed";

  const { analysis, current } = observation;
  const parts = analysis.usable
    ? ["Usable", analysis.cameraMovement]
    : ["Unusable", analysis.unusableReason].filter((part): part is string => Boolean(part));
  // Stale means the proxy was regenerated after this observation, so what the
  // model saw is not what plays now.
  if (!current) parts.push("stale");
  return parts.join(" · ");
}

/**
 * Whether the Analysis tab should carry its attention dot. A clip that has
 * never been analyzed is not a problem to flag — only a bad or outdated
 * verdict is.
 */
export function analysisNeedsAttention(observation: AssetAnalysisView | null): boolean {
  if (!observation) return false;
  return !observation.analysis.usable || !observation.current;
}

/**
 * Whether a clip's source media is reachable right now.
 *
 * Three states rather than two, because "the original is offline" is only a
 * problem in one of them. Reviewing a NAS project from a laptop on a train is
 * the *designed* mode — proxies exist for it — so an offline original with a
 * proxy is a fact to state quietly, while an offline original with no proxy is
 * a clip that cannot be played at all.
 */
export type MediaAvailability = "available" | "proxy_only" | "camera_proxy_only" | "blocked";

export function mediaAvailability(asset: Asset): MediaAvailability {
  if (asset.originalAvailable) return "available";
  if (asset.proxyAvailable) return "proxy_only";
  return asset.cameraProxyAvailable ? "camera_proxy_only" : "blocked";
}

/** What the offline mark means, for a tooltip and for the accessible name. */
export function mediaAvailabilityDescription(asset: Asset): string {
  switch (mediaAvailability(asset)) {
    case "proxy_only":
      return "Original is offline — reviewing from the 720p proxy";
    case "camera_proxy_only":
      return "Original is offline — reviewing from the verified camera sidecar";
    case "blocked":
      return "Original is offline and there is no proxy — this clip cannot be played";
    default:
      return "Original is mounted";
  }
}

/**
 * What to call the place this clip's original came from, when asking where it
 * went. The volume when the path names one, otherwise the containing folder:
 * "Where is the footage from HOKKAIDO_SSD now?" is answerable, and "where is
 * the footage from these clips now?" is not.
 */
export function sourceVolumeHint(asset: Asset): string {
  const segments = asset.path.split("/").filter(Boolean);
  if (segments[0] === "Volumes" && segments[1]) return segments[1];
  const parent = segments.at(-2);
  return parent ?? asset.filename;
}

export function fileSummary(asset: Asset): string {
  return `${asset.camera} · ${asset.codec.toUpperCase()} · ${formatBytes(asset.sizeBytes)}`;
}

export function descriptionSummary(asset: Asset): string {
  const firstLine = asset.description.split("\n")[0]?.trim() ?? "";
  return firstLine || "No description";
}

export function clockSummary(asset: Asset): string {
  const seconds = asset.offsetSeconds;
  if (!seconds) return "No offset";

  // U+2212 MINUS SIGN rather than a hyphen, matching the transport's −15.
  const sign = seconds < 0 ? "−" : "+";
  const magnitude = Math.abs(seconds);
  if (magnitude < 60) return `${sign}${magnitude} sec`;
  return `${sign}${Math.round(magnitude / 60)} min`;
}
