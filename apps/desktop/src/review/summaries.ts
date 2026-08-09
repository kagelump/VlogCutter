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
  rejected: "Discarded as unreliable",
  failed: "Transcription failed",
};

export function formatBytes(bytes: number) {
  if (bytes < 1_000_000_000) return `${(bytes / 1_000_000).toFixed(0)} MB`;
  if (bytes >= 1_000_000_000_000) return `${(bytes / 1_000_000_000_000).toFixed(1)} TB`;
  return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
}

export function transcriptSummary(transcript: ClipTranscript | null): string {
  if (!transcript) return "Not transcribed";

  const label = transcriptStatusLabels[transcript.status];
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
