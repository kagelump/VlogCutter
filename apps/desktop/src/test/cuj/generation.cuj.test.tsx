/**
 * CUJ 8 and 9: making review media, and losing it.
 *
 * The assertion that matters in every case here is the state diff. A batch that
 * reports three generated proxies must have changed exactly three assets, and
 * the clips it changed must become playable from the proxy. Miscounting is the
 * bug this file is looking for — the browser-preview mock reports two proxies
 * and changes nothing at all.
 */

import { screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, renderApp, type GenerationOutcome, type GenerationStep } from "../harness";

function halfProxied() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    // Two of five clips already have current review media.
    .camera("Sony A7C II", { clips: 5, withProxy: 2, startAt: "2026-02-08T00:10:00.000Z" })
    .build();
}

function proxiedCount(harness: { state: { assets: Record<string, { proxyAvailable: boolean }> } }) {
  return Object.values(harness.state.assets).filter((asset) => asset.proxyAvailable).length;
}

function currentSource() {
  return document.querySelector('video[data-cache-slot="current"]')?.getAttribute("src") ?? "";
}

/** The top bar offers the batch only while there is work outstanding. */
function makeProxiesButton() {
  return screen.getByRole("button", { name: /Make \d+ proxies/ });
}

describe("CUJ 8 — making the missing proxies", () => {
  it("changes exactly the assets it reports, and no others", async () => {
    const harness = halfProxied();
    const app = await renderApp(harness);
    expect(proxiedCount(harness)).toBe(2);

    await app.user.click(makeProxiesButton());

    await waitFor(() => expect(proxiedCount(harness)).toBe(5));
    // The message is the app's own accounting; state is the truth it must match.
    expect(await screen.findByText(/Made 3 proxies; 2 already ready/)).toBeVisible();
  });

  it("counts a proxy from an older recipe as work outstanding", async () => {
    const harness = desktopScenario()
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 3, withProxy: true })
      .withStaleProxy("asset_sony_a7c_ii_002")
      .build();
    const app = await renderApp(harness);

    // Two clips are current, one carries a `metadata_baked_v1` proxy.
    expect(makeProxiesButton()).toHaveTextContent("Make 1 proxies");

    await app.user.click(makeProxiesButton());
    await waitFor(() => expect(harness.state.assets.asset_sony_a7c_ii_002.proxyRepresentation).toBe("raw_v3"));
    expect(await screen.findByText(/Made 1 proxies; 2 already ready/)).toBeVisible();
  });

  it("makes each clip playable as the batch reaches it, not at the end", async () => {
    const harness = halfProxied();
    const app = await renderApp(harness);
    const job = harness.job<GenerationStep, GenerationOutcome>("generate_missing_proxies");

    await app.user.click(makeProxiesButton());
    await job.started();

    expect(harness.state.assets.asset_sony_a7c_ii_003.proxyAvailable).toBe(false);
    await job.progress({ assetId: "asset_sony_a7c_ii_003" });
    expect(harness.state.assets.asset_sony_a7c_ii_003.proxyAvailable).toBe(true);
    expect(harness.state.assets.asset_sony_a7c_ii_004.proxyAvailable).toBe(false);

    await job.progress({ assetId: "asset_sony_a7c_ii_004" });
    await job.progress({ assetId: "asset_sony_a7c_ii_005" });
    await job.complete();

    await waitFor(() => expect(proxiedCount(harness)).toBe(5));
  });

  it("shows progress as the batch reports it, with no timer anywhere", async () => {
    const harness = halfProxied();
    const app = await renderApp(harness);
    const job = harness.job<GenerationStep, GenerationOutcome>("generate_missing_proxies");

    await app.user.click(makeProxiesButton());
    await job.started();

    await job.progress({ assetId: "asset_sony_a7c_ii_003" });
    expect(await screen.findByText("1/3")).toBeVisible();
    await job.progress({ assetId: "asset_sony_a7c_ii_004" });
    expect(await screen.findByText("2/3")).toBeVisible();

    await job.complete();
    // The slot is not empty afterwards — it carries the completion message —
    // but the progress bar it shared with is gone.
    await waitFor(() => expect(screen.queryByRole("progressbar")).toBeNull());
  });

  it("keeps the clips it finished when the batch fails partway", async () => {
    const harness = halfProxied();
    const app = await renderApp(harness);
    const job = harness.job<GenerationStep, GenerationOutcome>("generate_missing_proxies");

    await app.user.click(makeProxiesButton());
    await job.started();

    await job.progress({ assetId: "asset_sony_a7c_ii_003" });
    await job.fail("The source volume disconnected");

    await waitFor(() => expect(screen.getByText(/source volume disconnected/i)).toBeVisible());
    // The finished prefix survives; the rest is untouched.
    expect(harness.state.assets.asset_sony_a7c_ii_003.proxyAvailable).toBe(true);
    expect(harness.state.assets.asset_sony_a7c_ii_004.proxyAvailable).toBe(false);
    expect(harness.state.assets.asset_sony_a7c_ii_005.proxyAvailable).toBe(false);
  });

  it("switches the selected clip from its original to the new proxy", async () => {
    const harness = halfProxied();
    const app = await renderApp(harness);

    await app.user.click(screen.getByText("SONY_003.MP4"));
    await waitFor(() => expect(currentSource()).toContain("rep=original"));

    await app.user.click(makeProxiesButton());

    await waitFor(() => expect(currentSource()).toContain("rep=proxy"));
    expect(harness.state.assets.asset_sony_a7c_ii_003.proxyPath).toBe(
      "/Users/harness/Movies/proxies/asset_sony_a7c_ii_003.v3.mp4",
    );
  });

  it("generates a single clip's proxy through its own command", async () => {
    // `generate_proxy` is reachable in production only from the stage's slate,
    // which appears when the selected clip has no playable URL at all — so it
    // is exercised here at the command boundary rather than through a screen
    // the fixture cannot legitimately reach.
    const harness = halfProxied();
    await renderApp(harness);

    const updated = (await window.__TAURI_INTERNALS__!.invoke("generate_proxy", {
      assetId: "asset_sony_a7c_ii_004",
    })) as { proxyAvailable: boolean; proxyRepresentation: string };

    expect(updated).toMatchObject({ proxyAvailable: true, proxyRepresentation: "raw_v3" });
    expect(harness.state.assets.asset_sony_a7c_ii_004.proxyAvailable).toBe(true);
    expect(proxiedCount(harness)).toBe(3);
  });

  it("generates thumbnails for every clip that lacks one", async () => {
    const harness = halfProxied();
    const app = await renderApp(harness);

    await app.user.click(screen.getByLabelText("More media generation actions"));
    await app.user.click(await screen.findByRole("button", { name: /thumbnails/i }));

    await waitFor(() =>
      expect(Object.values(harness.state.assets).every((asset) => asset.thumbnailQuality === 1)).toBe(true),
    );
    expect(harness.state.assets.asset_sony_a7c_ii_001.thumbnailPath).toBe(
      "/Users/harness/Movies/thumbnails/asset_sony_a7c_ii_001/q1.jpg",
    );
  });
});

describe("CUJ 9 — the source goes offline", () => {
  it("keeps a proxied clip playable when its original disappears", async () => {
    const harness = halfProxied();
    const app = await renderApp(harness);

    harness.asset("asset_sony_a7c_ii_001").setOriginalAvailable(false);
    // The app learns about it on the read it already performs after a write.
    await app.user.click(screen.getByLabelText("Frame orientation"));

    await waitFor(() => expect(harness.state.assets.asset_sony_a7c_ii_001.originalAvailable).toBe(false));
    expect(currentSource()).toContain("rep=proxy");
  });

  it("puts a clip with neither original nor proxy on the slate instead of the player", async () => {
    const harness = halfProxied();
    const app = await renderApp(harness);

    await app.user.click(screen.getByText("SONY_003.MP4"));
    harness.asset("asset_sony_a7c_ii_003").setOriginalAvailable(false);
    // Any write refreshes that asset, which is how the app finds out.
    await app.user.click(screen.getByLabelText("Frame orientation"));

    expect(await screen.findByText("No review proxy yet")).toBeVisible();
    expect(document.querySelector('video[data-cache-slot="current"]')).toBeNull();
  });

  it("skips offline originals when generating the missing proxies", async () => {
    const harness = halfProxied();
    const app = await renderApp(harness);

    harness.asset("asset_sony_a7c_ii_004").setOriginalAvailable(false);

    await app.user.click(makeProxiesButton());

    await waitFor(() => expect(screen.getByText(/Made 2 proxies/)).toBeVisible());
    expect(harness.state.assets.asset_sony_a7c_ii_004.proxyAvailable).toBe(false);
    expect(harness.state.assets.asset_sony_a7c_ii_005.proxyAvailable).toBe(true);
  });

  it("refuses to make a proxy from an original that is gone, and says why", async () => {
    const harness = halfProxied();
    await renderApp(harness);
    harness.asset("asset_sony_a7c_ii_003").setOriginalAvailable(false);

    await expect(
      window.__TAURI_INTERNALS__!.invoke("generate_proxy", { assetId: "asset_sony_a7c_ii_003" }),
    ).rejects.toThrow(/original for SONY_003\.MP4 is offline/i);
    expect(harness.state.assets.asset_sony_a7c_ii_003.proxyAvailable).toBe(false);
  });

  it("repairs the clips a relink folder actually contains, and no others", async () => {
    const harness = desktopScenario()
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 5, withProxy: 2 })
      .offlineOriginal("asset_sony_a7c_ii_003", "asset_sony_a7c_ii_004")
      .relinkableAt("/Volumes/Travel2", "asset_sony_a7c_ii_003")
      .build();
    await renderApp(harness);

    const report = (await window.__TAURI_INTERNALS__!.invoke("relink_folder", {
      projectId: "project_hokkaido",
      path: "/Volumes/Travel2",
    })) as { repaired: number; stillMissing: number };

    expect(report).toMatchObject({ repaired: 1, stillMissing: 1 });
    expect(harness.state.assets.asset_sony_a7c_ii_003.originalAvailable).toBe(true);
    expect(harness.state.assets.asset_sony_a7c_ii_004.originalAvailable).toBe(false);
  });
});
