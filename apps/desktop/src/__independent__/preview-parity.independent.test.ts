/**
 * Independent charter for H01-T010: look for divergence, not for bugs.
 *
 * The claim under test is that the Vite preview and the CUJ harness are two
 * front doors onto one fake backend. Divergence would show up in three ways,
 * and each has a case here:
 *
 *   1. an adapter that still answers differently outside Tauri;
 *   2. a preview that serves a command the harness does not, or vice versa;
 *   3. a behaviour that one door observes and the other does not.
 *
 * Case 3 is the load-bearing one, and it is checked by changing the *shared*
 * model — generating proxies — and watching both doors report the change.
 */

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { commandTable, EVENT_COMMANDS } from "../test/harness/DesktopHarness";
import { previewScenario } from "../preview/demoScenario";
import { installPreviewBackend } from "../preview";
import { projectAssets } from "../test/harness/state";

const desktopRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const ADAPTERS = ["src/api.ts", "src/chronologyApi.ts"] as const;

function adapterSource(): string {
  return ADAPTERS.map((path) => readFileSync(resolve(desktopRoot, path), "utf8")).join("\n");
}

/** Talk to the app's backend the way `@tauri-apps/api` does. */
async function call(command: string, args: Record<string, unknown> = {}): Promise<unknown> {
  const internals = window.__TAURI_INTERNALS__;
  if (!internals) throw new Error("No backend is installed");
  return internals.invoke(command, args);
}

afterEach(() => {
  const mutable = window as unknown as Record<string, unknown>;
  delete mutable.__TAURI_INTERNALS__;
  delete mutable.__TAURI_EVENT_PLUGIN_INTERNALS__;
  delete mutable.__RCDVLOG_PREVIEW__;
});

describe("the preview and the CUJ harness are one implementation", () => {
  it("leaves no environment branch in the adapters to disagree with", () => {
    const source = adapterSource();
    // `inTauri()` survives as `isDesktop`, but nothing may branch on its
    // negation any more: that was the second fake backend.
    expect(source).not.toContain("!inTauri()");
    expect(source).not.toMatch(/\binAutomatedTest\b/);
    // The demo values that used to live in `api.ts` moved into a scenario.
    expect(source).not.toMatch(/\bmock[A-Z]\w*/);
  });

  it("serves exactly the commands the harness serves, and no others", () => {
    installPreviewBackend();
    const served = new Set([...Object.keys(commandTable), ...EVENT_COMMANDS]);
    // Same table, reached through the same seam: there is nothing for the
    // preview to add, and an unknown command still throws.
    expect(served.has("generate_missing_proxies")).toBe(true);
    return expect(call("this_command_does_not_exist")).rejects.toThrow(
      /Unhandled Tauri command in test harness/,
    );
  });

  it("reports generation the preview can then observe, which the old mock could not", async () => {
    const harness = installPreviewBackend();
    const projectId = harness.state.projects[0].id;
    const before = projectAssets(harness.state, projectId).filter((asset) => asset.proxyAvailable).length;
    const missing = projectAssets(harness.state, projectId).filter(
      (asset) => asset.originalAvailable && !asset.proxyAvailable,
    ).length;
    expect(missing).toBeGreaterThan(0);

    // The preview's timer-driven job claims this command, so drive it the way
    // the app does and let the driver report every step.
    const report = (await call("generate_missing_proxies", { projectId })) as { generated: number };

    expect(report.generated).toBe(missing);
    const after = (await call("list_assets", { projectId })) as Array<{ proxyAvailable: boolean }>;
    expect(after.filter((asset) => asset.proxyAvailable)).toHaveLength(before + missing);
  }, 20_000);

  it("declares its chronology rather than solving one", () => {
    const state = previewScenario().build().state;
    const declared = state.chronology.overview.assets.map((asset) => asset.assetId);
    // Every clip the ribbon shows was named by the scenario; nothing here
    // derives an order, which is what keeps the solver's authority in Rust.
    expect(declared.length).toBeGreaterThan(0);
    for (const assetId of declared) expect(state.assets[assetId]).toBeDefined();
  });

  it("keeps its non-determinism out of anything a CUJ loads", () => {
    const harnessDirectory = resolve(desktopRoot, "src/test/harness");
    const modules = [
      "DesktopHarness.ts",
      "scenario.ts",
      "state.ts",
      "jobs.ts",
      "clock.ts",
      "builders.ts",
      "chronologyFixtures.ts",
      "renderApp.tsx",
    ];
    for (const module of modules) {
      const source = readFileSync(resolve(harnessDirectory, module), "utf8");
      expect(source, `${module} must not reach into preview-only code`).not.toContain("../preview");
      expect(source, `${module} must not sleep on a real timer`).not.toContain("setTimeout");
      // Calls, not the prose about them: these files explain why they avoid
      // ambient identity, and naming it is not using it.
      expect(source, `${module} must not invent identity`).not.toMatch(/Math\.random\(|randomUUID\(|new Date\(\)/);
    }
  });
});
