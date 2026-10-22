/**
 * Booting the real application against a harness.
 *
 * `App` is rendered unmodified. Its adapters take the Tauri branch because the
 * harness installed `window.__TAURI_INTERNALS__`, so nothing in a CUJ depends on
 * the browser-preview demo data inside `api.ts`.
 */

import { afterEach, expect } from "vitest";
import { act, cleanup, render, screen, waitFor, within, type RenderResult } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "../../App";
import type { ActWrapper, DesktopHarness } from "./DesktopHarness";
import { projectAssets } from "./state";

let activeHarness: DesktopHarness | null = null;

/**
 * The test-side act wrapper.
 *
 * The harness itself has no static import of `@testing-library/react` — the
 * browser preview installs the same harness and must not carry a devDependency
 * — so the version that flushes React lives here, on the door only tests use.
 */
const actWrapper: ActWrapper = async (action) => {
  let result!: Awaited<ReturnType<typeof action>>;
  await act(async () => {
    result = await action();
  });
  return result;
};

// Registered when a test file imports this helper, so a harness can never leak
// its window globals into the next test.
afterEach(() => {
  cleanup();
  activeHarness?.uninstall();
  activeHarness = null;
});

export interface BootedApp extends RenderResult {
  harness: DesktopHarness;
  user: ReturnType<typeof userEvent.setup>;
  /**
   * Unmount and render again against the same harness state — the renderer
   * restart case. This proves what the frontend restores from the backend; it
   * says nothing about SQLite durability, which Rust tests own.
   */
  remount(): Promise<BootedApp>;
}

/** Boot and wait until the shell and the first clip are on screen. */
export async function renderApp(
  harness: DesktopHarness,
  options: { waitFor?: "ready" | "shell" } = {},
): Promise<BootedApp> {
  harness.useActWrapper(actWrapper);
  if (!harness.installed) harness.install();
  activeHarness = harness;

  const rendered = render(<App />);
  await settle(harness, options);

  const user = userEvent.setup();
  const booted: BootedApp = {
    ...rendered,
    harness,
    user,
    remount: async () => {
      rendered.unmount();
      return renderApp(harness, options);
    },
  };
  return booted;
}

/**
 * Wait for the boot reads to land.
 *
 * The context write is deliberately included: `App` schedules it 100 ms after
 * a project opens and again when the selection resolves. Wait for the write
 * containing that stable selection, not merely the initial null-valued write;
 * finishing before it fires would leave a timer updating an unmounted tree.
 */
export async function settle(harness: DesktopHarness, options: { waitFor?: "ready" | "shell" } = {}) {
  await screen.findByTestId("operation-slot");
  if (options.waitFor === "shell") return;
  if (harness.state.projects.length > 0) {
    const projectId = harness.state.projects[0].id;
    const visibleAssets = projectAssets(harness.state, projectId).filter((asset) => !asset.removed);
    await waitFor(() => expect(harness.calls("list_assets").length).toBeGreaterThan(0));
    await waitFor(() => {
      const latestRead = harness.commandLog()
        .filter((entry) => entry.command === "list_assets" && entry.args.projectId === projectId)
        .at(-1);
      if (latestRead?.outcome === "rejected") {
        // A failed initial read has no queue selection to persist. Treat it as
        // settled only once the retry affordance reflects that terminal result.
        expect(screen.getByRole("button", { name: "Try again" })).toBeVisible();
        return;
      }
      if (visibleAssets.length === 0) {
        expect(harness.calls("record_desktop_context")).toContainEqual({ projectId, assetId: null });
        return;
      }
      const selected = selectedRow();
      expect(selected).toBeDefined();
      const position = Number(selected?.getAttribute("aria-posinset"));
      const selectedAsset = visibleAssets[position - 1];
      expect(selectedAsset).toBeDefined();
      expect(harness.calls("record_desktop_context")).toContainEqual({
        projectId,
        assetId: selectedAsset.id,
      });
    }, {
      timeout: 2_000,
    });
  }
}

/**
 * The review queue's rows, in order.
 *
 * Scoped to the "Clips" listbox on purpose: the project picker is a listbox of
 * options too, and an unscoped `getAllByRole("option")` silently includes it.
 */
export function queueRows(): HTMLElement[] {
  return within(screen.getByRole("listbox", { name: "Clips" })).queryAllByRole("option");
}

/**
 * The filename of each queue row, in order.
 *
 * Read from the row's composed accessible name (`Clip 3, SONY_001.MP4, …`)
 * rather than from what is currently on the secondary line: that line shows a
 * filename, a camera, or a search-match snippet depending on what is being
 * asked, and a CUJ talks about clips by filename throughout.
 */
export function queueFilenames(): string[] {
  return queueRows().map((row) => (row.getAttribute("aria-label") ?? "").split(", ")[1] ?? "");
}

/** The headline each queue row leads with: the clip's title, or its filename. */
export function queueHeadlines(): string[] {
  return queueRows().map((row) => row.querySelector(".queue-main strong")?.textContent?.trim() ?? "");
}

export function selectedRow(): HTMLElement | undefined {
  return queueRows().find((row) => row.getAttribute("aria-selected") === "true");
}
