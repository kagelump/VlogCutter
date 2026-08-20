// Pure helpers for the dev-with-logs wrapper. No I/O here so they stay testable.

export const TOTAL_CAP_BYTES = 100 * 1024 * 1024;
export const FILE_CAP_BYTES = 90 * 1024 * 1024;

// Oldest-first FIFO prune: return the names to delete so the remaining files
// sum to <= totalCapBytes. Already under cap => delete nothing.
export function planPrune(files, totalCapBytes) {
  const ordered = [...files].sort((a, b) => a.mtimeMs - b.mtimeMs);
  let total = ordered.reduce((sum, f) => sum + f.size, 0);
  const toDelete = [];
  for (const file of ordered) {
    if (total <= totalCapBytes) break;
    toDelete.push(file.name);
    total -= file.size;
  }
  return toDelete;
}

function pad(value, width = 2) {
  return String(value).padStart(width, "0");
}

export function sessionFileName(date = new Date()) {
  const stamp =
    `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}` +
    `-${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`;
  return `dev-${stamp}.log`;
}
