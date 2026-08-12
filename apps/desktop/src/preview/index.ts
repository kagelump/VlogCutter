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
import { previewScenario } from "./demoScenario";
import { previewConvertFileSrc } from "./previewFrames";
import { startPreviewJobs } from "./liveJobs";

declare global {
  interface Window {
    /** Preview-only handle, for poking at the demo state from the console. */
    __RCDVLOG_PREVIEW__?: DesktopHarness;
  }
}

export function installPreviewBackend(): DesktopHarness {
  const harness = previewScenario().build({ convertFileSrc: previewConvertFileSrc });
  harness.install();
  startPreviewJobs(harness);
  window.__RCDVLOG_PREVIEW__ = harness;
  return harness;
}

export { previewScenario } from "./demoScenario";
