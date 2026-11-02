// Parse a pasted YouTube link into its 11-character video ID. Pure string
// parsing: the link is never fetched, embedded, or treated as proof of what
// YouTube or the TV plays. The server accepts only the bare ID.

const VIDEO_ID = /^[A-Za-z0-9_-]{11}$/;
const HOSTS = new Set(["youtube.com", "www.youtube.com", "m.youtube.com", "music.youtube.com", "youtube-nocookie.com", "www.youtube-nocookie.com"]);
const PATH_PREFIXES = ["/shorts/", "/live/", "/embed/", "/v/"];

export function parseYoutubeVideoId(input: string): string | null {
  const value = input.trim();
  if (!value) return null;
  if (VIDEO_ID.test(value)) return value;
  let url: URL;
  try {
    url = new URL(/^[a-z]+:\/\//i.test(value) ? value : `https://${value}`);
  } catch {
    return null;
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") return null;
  const host = url.hostname.toLowerCase();
  let candidate: string | null = null;
  if (host === "youtu.be") {
    candidate = url.pathname.slice(1).split("/")[0] ?? null;
  } else if (HOSTS.has(host)) {
    if (url.pathname === "/watch") candidate = url.searchParams.get("v");
    else {
      const prefix = PATH_PREFIXES.find((p) => url.pathname.startsWith(p));
      if (prefix) candidate = url.pathname.slice(prefix.length).split("/")[0] ?? null;
    }
  }
  return candidate && VIDEO_ID.test(candidate) ? candidate : null;
}

/** Accept the CLI success envelope or its `data` member; return the handoff. */
/** A #232 historical rendered-artifact review, recognized by its purpose discriminator. */
export function isHistoricalReview(handoff: Record<string, unknown>): boolean {
  return handoff.purpose === "historicalRenderedArtifactReview";
}

export function unwrapHandoff(parsed: unknown): Record<string, unknown> | null {
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
  const record = parsed as Record<string, unknown>;
  if (record.ok === true && (record.command === "edit-plan.review-target-export" || record.command === "edit-plan.historical-review-target-export") && record.data && typeof record.data === "object") {
    return record.data as Record<string, unknown>;
  }
  return record;
}
