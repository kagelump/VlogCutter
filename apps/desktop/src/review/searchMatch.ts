import type { Asset, AssetAnalysisIndexEntry, TranscriptSegment } from "../types";

/**
 * Why a clip matched the search, and where in it (issue #12).
 *
 * Finding the clip is half the task. A row that says only "this one matched"
 * makes the reader open it to find out whether it matched a note they wrote,
 * a word that was spoken, or the camera's name — and the last of those is
 * almost never what they meant.
 */

/**
 * The fields searched, in the order a match in them explains the result.
 *
 * Deliberate human writing first, then speech, then everything a clip carries
 * whether anyone thought about it or not. A phrase that appears in both a note
 * and the filename is being remembered from the note.
 */
export const MATCH_FIELDS = [
  "note",
  "transcript",
  "title",
  "label",
  "description",
  "filename",
  "camera",
] as const;

export type MatchField = (typeof MATCH_FIELDS)[number];

export interface SearchMatch {
  field: MatchField;
  /** A window of the field's text around the match, elided at both ends. */
  text: string;
  /** Where the match starts within `text`, so the interface can mark it. */
  offset: number;
  length: number;
}

const WINDOW = 56;
const ELLIPSIS = "…";

/** A window of `value` around the match, elided where it was cut. */
export function snippetAround(
  value: string,
  start: number,
  window = WINDOW,
): { text: string; offset: number } {
  if (value.length <= window) return { text: value, offset: start };
  // Keep a third of the window ahead of the match: reading resumes forwards.
  const lead = Math.max(0, Math.min(value.length - window, start - Math.floor(window / 3)));
  const tail = Math.min(value.length, lead + window);
  const head = lead > 0 ? ELLIPSIS : "";
  const foot = tail < value.length ? ELLIPSIS : "";
  return { text: `${head}${value.slice(lead, tail)}${foot}`, offset: start - lead + head.length };
}

/**
 * The strongest match for `query` in one clip, or `null` when it does not
 * match. The clip's labels are searched as one joined value so a match in the
 * third label still reads as a label match.
 */
export function searchMatch(
  asset: Asset,
  entry: AssetAnalysisIndexEntry | undefined,
  query: string,
): SearchMatch | null {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return null;

  const values: Record<MatchField, string> = {
    note: asset.notes,
    transcript: entry?.transcriptText ?? "",
    title: asset.title,
    label: asset.labels.join(" · "),
    description: asset.description,
    filename: asset.filename,
    camera: asset.camera,
  };

  for (const field of MATCH_FIELDS) {
    const value = values[field];
    const at = value.toLocaleLowerCase().indexOf(needle);
    if (at === -1) continue;
    const { text, offset } = snippetAround(value, at);
    return { field, text, offset, length: needle.length };
  }
  return null;
}

/** Which transcript segments contain `query`, in playback order. */
export function transcriptMatches(
  segments: readonly TranscriptSegment[],
  query: string,
): number[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [];
  const found: number[] = [];
  segments.forEach((segment, index) => {
    if (segment.text.toLocaleLowerCase().includes(needle)) found.push(index);
  });
  return found;
}

/**
 * The next match to jump to, cycling back to the first.
 *
 * Cycling rather than stopping at the end: "say that again" is the reason
 * anyone presses this twice, and a control that goes dead on the last match
 * makes the reader work out which one they are on.
 */
export function nextMatch(matches: readonly number[], current: number): number | null {
  if (!matches.length) return null;
  const position = matches.indexOf(current);
  return matches[(position + 1) % matches.length];
}
