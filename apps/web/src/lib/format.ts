/** `1:05`, `1:02:03`. Whole seconds, for durations and recording clocks. */
export function formatClock(seconds: number) {
  const whole = Math.max(0, Math.floor(seconds));
  const h = Math.floor(whole / 3600);
  const m = Math.floor((whole % 3600) / 60);
  const s = whole % 60;
  return h ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

/** `12:04.5`. Tenths, for transcript timestamps. */
export function formatStamp(seconds: number) {
  const value = Math.max(0, seconds);
  const m = Math.floor(value / 60);
  const s = value - m * 60;
  return `${m}:${s.toFixed(1).padStart(4, "0")}`;
}

/** `12:04.52`. Hundredths, for cut boundaries. */
export function formatMovieTime(seconds: number) {
  const value = Math.max(0, seconds);
  const m = Math.floor(value / 60);
  const s = value - m * 60;
  return `${m}:${s.toFixed(2).padStart(5, "0")}`;
}

export function formatBytes(value: number | null | undefined) {
  if (value == null) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let amount = value;
  let unit = 0;
  while (amount >= 1000 && unit < units.length - 1) { amount /= 1000; unit += 1; }
  return `${amount.toFixed(unit && amount < 100 ? 1 : 0)} ${units[unit]}`;
}

export function formatEta(seconds: number | null | undefined) {
  if (seconds == null) return "estimating";
  const rounded = Math.max(0, Math.round(seconds));
  if (rounded < 60) return `${rounded}s`;
  const minutes = Math.floor(rounded / 60);
  if (minutes < 60) return `${minutes}m ${rounded % 60}s`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
const dateOnly = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });

export function formatDateTime(iso: string | null | undefined) {
  if (!iso) return "—";
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? iso : dateTime.format(date);
}

export function formatDate(iso: string | null | undefined) {
  if (!iso) return "—";
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? iso : dateOnly.format(date);
}

const relative = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });

export function formatRelative(iso: string | null | undefined, now = Date.now()) {
  if (!iso) return "never";
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return iso;
  const seconds = Math.round((then - now) / 1000);
  const abs = Math.abs(seconds);
  if (abs < 45) return "just now";
  if (abs < 3600) return relative.format(Math.round(seconds / 60), "minute");
  if (abs < 86_400) return relative.format(Math.round(seconds / 3600), "hour");
  if (abs < 86_400 * 30) return relative.format(Math.round(seconds / 86_400), "day");
  return formatDate(iso);
}

export function plural(count: number, one: string, many = `${one}s`) {
  return `${count.toLocaleString()} ${count === 1 ? one : many}`;
}

/**
 * Opaque IDs (`project_f298ae3a…`, `editrev_9ac7…`) shortened to the first
 * eight characters after the prefix. The full value stays available as a title.
 */
export function shortId(id: string, length = 8) {
  const body = id.includes("_") ? id.slice(id.indexOf("_") + 1) : id;
  return body.slice(0, length);
}

export function shortDigest(digest: string) {
  return digest.slice(0, 12);
}

function pad(value: number) {
  return String(value).padStart(2, "0");
}
