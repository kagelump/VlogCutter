/**
 * Booting the real application against a harness.
 *
 * `App` is rendered unmodified. Its adapters take the Tauri branch because the
 * harness installed `window.__TAURI_INTERNALS__`, so nothing in a CUJ depends on
 * the browser-preview demo data inside `api.ts`.
 */

import { afterEach, expect } from "vitest";
import { cleanup, render, screen, waitFor, type RenderResult } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "../../App";
import type { DesktopHarness } from "./DesktopHarness";

let activeHarness: DesktopHarness | null = null;

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
export async function renderApp(harness: DesktopHarness): Promise<BootedApp> {
  if (!harness.installed) harness.install();
  activeHarness = harness;

  const rendered = render(<App />);
  await settle(harness);

  const user = userEvent.setup();
  const booted: BootedApp = {
    ...rendered,
    harness,
    user,
    remount: async () => {
      rendered.unmount();
      return renderApp(harness);
    },
  };
  return booted;
}

/**
 * Wait for the boot reads to land.
 *
 * The context write is deliberately included: `App` schedules it 100 ms after a
 * project opens, and a test that finished before it fired would leave a timer
 * updating an unmounted tree.
 */
export async function settle(harness: DesktopHarness) {
  await screen.findByTestId("operation-slot");
  if (harness.state.projects.length > 0) {
    await waitFor(() => expect(harness.calls("list_assets").length).toBeGreaterThan(0));
    await waitFor(() => expect(harness.calls("record_desktop_context").length).toBeGreaterThan(0), {
      timeout: 2_000,
    });
  }
}

/** The queue rows currently on screen, in order. */
export function queueFilenames(): string[] {
  return screen
    .getAllByRole("option")
    .map((row) => row.querySelector(".queue-filename")?.textContent ?? row.textContent ?? "")
    .map((text) => text.trim());
}
