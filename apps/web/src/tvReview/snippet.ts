import { REVIEW_SNIPPET_MAX_SECONDS } from "@rcdvlog/project-client";

export interface FrameSpan { startFrame: number; endFrame: number }

/**
 * Split stored ranges into listening windows of at most the server's snippet
 * bound. Windows never cross a gap, so every one is fully stored audio.
 */
export function snippetWindows(ranges: FrameSpan[], sampleRate: number, maxSeconds = REVIEW_SNIPPET_MAX_SECONDS): FrameSpan[] {
  const max = Math.max(1, Math.floor(sampleRate * maxSeconds));
  const windows: FrameSpan[] = [];
  for (const range of ranges) {
    for (let start = range.startFrame; start < range.endFrame; start += max) {
      windows.push({ startFrame: start, endFrame: Math.min(range.endFrame, start + max) });
    }
  }
  return windows;
}

/**
 * The window to play for an utterance: its frames with a little context,
 * clipped to the one stored range that contains its start and to the bound.
 * Returns null when the utterance's start was never stored.
 */
export function utteranceWindow(ranges: FrameSpan[], sampleRate: number, startFrame: number, endFrame: number, contextSeconds = 0.5, maxSeconds = REVIEW_SNIPPET_MAX_SECONDS): FrameSpan | null {
  const range = ranges.find((r) => r.startFrame <= startFrame && startFrame < r.endFrame);
  if (!range) return null;
  const context = Math.round(sampleRate * contextSeconds);
  const start = Math.max(range.startFrame, startFrame - context);
  const end = Math.min(range.endFrame, Math.max(endFrame, startFrame + 1) + context, start + Math.floor(sampleRate * maxSeconds));
  return end > start ? { startFrame: start, endFrame: end } : null;
}
