import type { AnalysisStage, Asset } from "../types";

/**
 * Display formatters shared by the shell and its components.
 *
 * These lived in App.tsx and were copied into components as they were
 * extracted — `formatDuration` existed twice and `formatTimecode` twice, in
 * versions that agreed by luck rather than by construction. One definition
 * each, tested here, so a change to how a timecode reads is a change in one
 * place.
 */

export const analysisStageLabels: Record<AnalysisStage, string> = {
  proxies: "Making proxies",
  transcripts: "Transcribing",
  analysis: "Analyzing clips",
};

/**
 * Sync state as a person would say it. Unknown states pass through rather
 * than becoming "Unknown": a state the UI has not learned yet is still more
 * informative named than blanked.
 */
export function syncStateLabel(state: string): string {
  switch (state) {
    case "unpaired": return "Unpaired";
    case "offline": return "Offline";
    case "connecting": return "Connecting…";
    case "syncing": return "Syncing…";
    case "synced": return "Synced";
    case "attention": return "Needs attention";
    case "incompatible": return "Incompatible";
    case "credential_revoked": return "Credential revoked";
    default: return state;
  }
}

/** The clip's display name: what analysis called it, else its filename. */
export function clipTitle(asset: Asset) {
  return asset.title.trim() || asset.filename;
}

export function formatCost(usd: number) {
  // A non-finite value reaches here when the budget field is empty or garbage;
  // rendering "$NaN" beside a spend figure is worse than saying nothing.
  if (!Number.isFinite(usd)) return "the run budget";
  if (usd <= 0) return "$0.00";
  if (usd < 0.01) return "<$0.01";
  return `$${usd.toFixed(2)}`;
}

/** A position in milliseconds as `m:ss`. */
export function formatTimecode(ms: number) {
  const total = Math.max(0, Math.round(ms / 1000));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

/** A span in seconds as `m:ss`, truncated so a clip never reads a second long. */
export function formatDuration(seconds: number) {
  const safe = Number.isFinite(seconds) ? Math.max(0, seconds) : 0;
  const minutes = Math.floor(safe / 60);
  const remainder = Math.floor(safe % 60);
  return `${minutes}:${remainder.toString().padStart(2, "0")}`;
}

export function formatEta(seconds: number | null) {
  if (seconds === null || !Number.isFinite(seconds)) return "Estimating…";
  const rounded = Math.max(0, Math.ceil(seconds));
  if (rounded < 60) return `${rounded}s remaining`;
  const minutes = Math.ceil(rounded / 60);
  if (minutes < 60) return `${minutes}m remaining`;
  const hours = Math.floor(minutes / 60);
  const remainder = minutes % 60;
  return `${hours}h ${remainder}m remaining`;
}

/**
 * Camera time, to the second and without a year: a trip's clips share a year,
 * so it is the one field that never distinguishes two of them.
 */
export function formatDate(value: string | null) {
  if (!value) return "No camera time";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(date);
}
