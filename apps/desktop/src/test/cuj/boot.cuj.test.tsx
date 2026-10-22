/**
 * The seam itself.
 *
 * Everything else in this directory assumes the real `App` can boot through the
 * real `api.ts` into a fake Tauri runtime. This file is the proof, and it is
 * also where the harness's own guarantees are asserted: unknown commands throw,
 * writes are observable, listeners are torn down, and fixtures are stable.
 */

import { act, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { api } from "../../api";
import { desktopScenario, queueFilenames, renderApp } from "../harness";

function hokkaido() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 3, withProxy: true, startAt: "2026-02-08T00:14:08.000Z" })
    .camera("DJI Pocket 3", { clips: 2, startAt: "2026-02-08T00:18:41.000Z" })
    .build();
}

function bootProjectWithClips(clipCount: number) {
  if (clipCount === 5) return hokkaido();
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Antarctica field test", { id: "project_antarctica" })
    .camera("Sony A7C II", { clips: 200, withProxy: true, startAt: "2026-02-01T08:00:00.000Z", spacingSeconds: 600 })
    .camera("DJI Pocket 3", { clips: 200, withProxy: true, startAt: "2026-02-01T08:02:00.000Z", spacingSeconds: 600 })
    .camera("Insta360 X5", { clips: 100, startAt: "2026-02-01T08:04:00.000Z", spacingSeconds: 1_200 })
    .camera("Apple iPhone 17 Pro", { clips: 100, startAt: "2026-02-01T08:06:00.000Z", spacingSeconds: 1_200 })
    .build();
}

describe("booting the desktop against the harness", () => {
  it("renders the real App through the real adapters", async () => {
    const harness = hokkaido();
    await renderApp(harness);

    // The production check for "am I inside Tauri" passes, which is what makes
    // the browser-preview branches unreachable from a CUJ.
    expect(api.isDesktop()).toBe(true);

    expect(await screen.findByText("SONY_001.MP4")).toBeVisible();
    expect(screen.getByText("DJI_001.MP4")).toBeVisible();
  });

  it("issues its boot reads through named commands with the production argument shapes", async () => {
    const harness = hokkaido();
    await renderApp(harness);

    const issued = new Set(harness.commandNames());
    for (const command of [
      "prerequisite_status",
      "list_projects",
      "project_library",
      "desktop_settings",
      "server_connection_status",
      "list_assets",
      "chronology_overview",
      "project_analysis_index",
      "analysis_readiness",
      "model_status",
      "openrouter_key_present",
      "sync_status",
      "detect_mounted_cards",
      "record_desktop_context",
    ]) {
      expect(issued, `boot should issue ${command}`).toContain(command);
    }

    expect(harness.calls("list_assets")[0]).toEqual({ projectId: "project_hokkaido" });
    // `renderApp` settles on the stable selection write, not the initial
    // debounced write that can legitimately run before the asset read lands.
    expect(harness.calls("record_desktop_context").at(-1)).toEqual({
      projectId: "project_hokkaido",
      assetId: "asset_sony_a7c_ii_001",
    });
  });

  for (const clipCount of [5, 600]) {
    it(
      `waits for the queue selection when initial context is null (${clipCount} clips)`,
      async () => {
        const harness = bootProjectWithClips(clipCount);
        const projectId = clipCount === 5 ? "project_hokkaido" : "project_antarctica";
        harness.next("list_assets").delayUntilReleased();

        let settled = false;
        const boot = renderApp(harness).then((app) => {
          settled = true;
          return app;
        });

        await waitFor(() => {
          expect(harness.calls("record_desktop_context")).toContainEqual({
            projectId,
            assetId: null,
          });
        });
        // The null write is observable while the catalog read is still held.
        // Boot must not settle until the queue selection has been persisted.
        expect(settled).toBe(false);

        await act(async () => harness.release("list_assets"));
        await boot;

        const filenames = queueFilenames();
        expect(filenames).toHaveLength(clipCount);
        expect(filenames[0]).toBe("SONY_001.MP4");
        expect(harness.calls("record_desktop_context").at(-1)).toEqual({
          projectId,
          assetId: "asset_sony_a7c_ii_001",
        });
      },
      clipCount === 600 ? 30_000 : undefined,
    );
  }

  it("settles with a null context when a project has only removed clips", async () => {
    const harness = desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
      .project("Archived project", { id: "project_archived" })
      .camera("Sony A7C II", { clips: 1, withProxy: true })
      .removed("asset_sony_a7c_ii_001")
      .build();

    await renderApp(harness);

    expect(queueFilenames()).toEqual([]);
    expect(harness.calls("record_desktop_context").at(-1)).toEqual({
      projectId: "project_archived",
      assetId: null,
    });
  });

  it("records the desktop context write against the harness clock, not wall time", async () => {
    const harness = hokkaido();
    await renderApp(harness);

    expect(harness.state.settings.lastContextUpdatedAt).toBe("2026-08-09T12:00:00.000Z");
    expect(harness.state.settings.lastProjectId).toBe("project_hokkaido");
  });

  it("refuses a command it does not implement, naming it", async () => {
    const harness = hokkaido();
    await renderApp(harness);

    await expect(
      window.__TAURI_INTERNALS__!.invoke("a_command_nobody_implemented", {}),
    ).rejects.toThrow(/Unhandled Tauri command in test harness: a_command_nobody_implemented/);
  });

  it("makes a write observable by the next read", async () => {
    const harness = hokkaido();
    await renderApp(harness);

    await window.__TAURI_INTERNALS__!.invoke("set_removed", {
      assetId: "asset_dji_pocket_3_002",
      removed: true,
    });

    expect(harness.state.assets.asset_dji_pocket_3_002.removed).toBe(true);
    const assets = (await window.__TAURI_INTERNALS__!.invoke("list_assets", {
      projectId: "project_hokkaido",
    })) as Array<{ id: string; removed: boolean }>;
    expect(assets.find((asset) => asset.id === "asset_dji_pocket_3_002")?.removed).toBe(true);
  });

  it("delivers events to the application's real listeners and stops on unmount", async () => {
    const harness = hokkaido();
    const app = await renderApp(harness);

    // Five subscriptions are attached by App's effect: proxy, thumbnail,
    // analysis, model install, and import progress. Sync status is a sixth.
    expect(harness.events.listenerCount("proxy-progress")).toBe(1);
    expect(harness.events.listenerCount("sync-status-changed")).toBe(1);

    expect(await harness.emit("proxy-progress", { asset: null, estimate: null })).toBe(1);

    app.unmount();
    await waitFor(() => expect(harness.events.listenerCount("proxy-progress")).toBe(0));
    expect(await harness.emit("proxy-progress", { asset: null, estimate: null })).toBe(0);
  });

  it("builds the same fixtures on every run", async () => {
    const first = hokkaido();
    const second = hokkaido();
    expect(second.state.assets).toEqual(first.state.assets);
    expect(second.state.projects).toEqual(first.state.projects);
  });

  it("serves review media from the production URL builder", async () => {
    const harness = hokkaido();
    await renderApp(harness);

    const video = document.querySelector<HTMLVideoElement>('video[data-asset-id="asset_sony_a7c_ii_001"]');
    expect(video?.getAttribute("src")).toBe(
      "http://127.0.0.1:54321/media/asset_sony_a7c_ii_001?rep=proxy&t=harness-media-token",
    );
  });

  it("falls back to the original when a clip has no proxy", async () => {
    const harness = hokkaido();
    const app = await renderApp(harness);

    await app.user.click(screen.getByText("DJI_001.MP4"));
    await waitFor(() =>
      expect(
        document.querySelector<HTMLVideoElement>('video[data-asset-id="asset_dji_pocket_3_001"][data-cache-slot="current"]')
          ?.getAttribute("src"),
      ).toBe("http://127.0.0.1:54321/media/asset_dji_pocket_3_001?rep=original&t=harness-media-token"),
    );
  });
});
