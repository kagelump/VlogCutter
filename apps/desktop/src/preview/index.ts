/**
 * The browser preview's backend.
 *
 * `main.tsx` loads this module — and only this module — when the app boots
 * outside Tauri, through a dynamic `import()` so it lands in its own chunk and
 * never in the shipped desktop bundle.
 *
 * Installing it makes `inTauri()` true, which is the point: from here on the
 * preview runs the real `api.ts` and `chronologyApi.ts` against the same
 * in-memory backend the CUJ suite uses. There is no second set of mocks to keep
 * in step, and a behaviour change in `src/test/harness/commands/` is visible
 * through both doors.
 *
 * What the preview still is not: evidence. It proves nothing about SQLite,
 * media decoding, Keychain, native dialogs, or FFmpeg — see
 * `docs/integration-testing.md`.
 */

import { DesktopHarness } from "../test/harness/DesktopHarness";
import { previewScenario, reviewRequiredScenario, scaleScenario } from "./demoScenario";
import { previewConvertFileSrc } from "./previewFrames";
import { startPreviewJobs } from "./liveJobs";

declare global {
  interface Window {
    /** Preview-only handle, for poking at the demo state from the console. */
    __RCDVLOG_PREVIEW__?: DesktopHarness;
  }
}

/**
 * How many clips the preview should seed, from `?clips=` in the URL.
 *
 * The demo project is fourteen clips, chosen to make every surface legible.
 * Nothing in it can answer "what does this cost at the scale AGENTS.md §1
 * sets" — thousands of clips — and that is a real-engine question: mounting
 * three thousand rows is a reconciliation and layout cost jsdom does not have.
 * So the preview takes a size, and the browser tier asks the question.
 */
function requestedClipCount(search: string): number | null {
  const value = new URLSearchParams(search).get("clips");
  if (value === null) return null;
  const clips = Number(value);
  return Number.isInteger(clips) && clips > 0 && clips <= 20_000 ? clips : null;
}

function requestedReviewFixture(search: string): boolean {
  return new URLSearchParams(search).get("review") === "required";
}

export function installPreviewBackend(): DesktopHarness {
  const clips = typeof window === "undefined" ? null : requestedClipCount(window.location.search);
  const reviewFixture =
    typeof window !== "undefined" && requestedReviewFixture(window.location.search);
  const scenario = reviewFixture
    ? reviewRequiredScenario()
    : clips === null
      ? previewScenario()
      : scaleScenario(clips);
  const harness = scenario.build({ convertFileSrc: previewConvertFileSrc });
  harness.install();
  // The demo's live jobs are staged against the demo's clips; a scale or
  // isolated review fixture is about its declared state, not the job timer.
  if (!reviewFixture && clips === null) startPreviewJobs(harness);
  window.__RCDVLOG_PREVIEW__ = harness;
  return harness;
}

export { previewScenario, reviewRequiredScenario, scaleScenario } from "./demoScenario";
export { requestedClipCount, requestedReviewFixture };
