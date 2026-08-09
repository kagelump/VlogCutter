/**
 * Playback orchestration.
 *
 * Every media event here is a real DOM event dispatched at the real `<video>`
 * the app mounted. What that proves is orchestration: which element is active,
 * what the transport shows, how buffering and position are tracked, what the
 * app does when an element fails.
 *
 * What it cannot prove — and what no assertion in this file claims — is that
 * anything decoded, that a first frame painted, or that an unbuffered seek
 * works. Those are `docs/tasks/desktop/MEDIA_DELIVERY.md`'s native acceptance
 * criteria and they stay in the native tier.
 */

import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, renderApp } from "../harness";

/** Autoplay off, so the transport's state is the test's to set. */
function session() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 3, withProxy: true, durationSeconds: 24, startAt: "2026-02-08T00:10:00.000Z" })
    .settings({ autoplayMode: "never" })
    .build();
}

const first = "asset_sony_a7c_ii_001";
const second = "asset_sony_a7c_ii_002";

/** The transport's own clock, as opposed to the per-row durations in the queue. */
function transportTime(): string {
  return document.querySelector(".transport-time")?.textContent?.trim() ?? "";
}

function stageSlate(): HTMLElement | null {
  return document.querySelector(".offline-slate");
}

describe("playback orchestration", () => {
  it("takes the duration from the element that actually loaded", async () => {
    const harness = session();
    await renderApp(harness);

    await harness.media(first).loaded({ duration: 24.5 });

    await waitFor(() => expect(transportTime()).toBe("0:00 / 0:24"));
  });

  it("ignores metadata from a preloaded neighbour", async () => {
    const harness = session();
    await renderApp(harness);

    await harness.media(first).loaded({ duration: 24 });
    // The neighbour is mounted and fires its own events; the transport belongs
    // to the selection.
    await harness.media(second).loaded({ duration: 300 });

    expect(transportTime()).toBe("0:00 / 0:24");
  });

  it("follows the element through play, buffering, and pause", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });

    await app.user.click(screen.getByLabelText("Play"));
    await harness.media(first).playing();
    expect(await screen.findByLabelText("Pause")).toBeVisible();

    await harness.media(first).buffering();
    expect(await screen.findByRole("status")).toHaveTextContent("Buffering");

    await harness.media(first).playing();
    await waitFor(() => expect(screen.queryByRole("status")).toBeNull());

    await app.user.click(screen.getByLabelText("Pause"));
    await harness.media(first).paused();
    expect(await screen.findByLabelText("Play")).toBeVisible();
  });

  it("tracks the position the element reports", async () => {
    const harness = session();
    await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });

    await harness.media(first).advanceTo(12);

    await waitFor(() => expect(transportTime()).toBe("0:12 / 0:24"));
    expect(screen.getByLabelText("Video position")).toHaveValue("12");
  });

  it("starts the next clip from its own beginning", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });
    await harness.media(first).advanceTo(18);

    await app.user.click(screen.getByLabelText("Next clip"));
    await harness.media(second).loaded({ duration: 24, currentTime: 0 });

    expect(screen.getByLabelText("Video position")).toHaveValue("0");
  });

  it("autoplays a clip with review media when the setting asks for it", async () => {
    const harness = desktopScenario()
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 2, withProxy: 1 })
      .settings({ autoplayMode: "proxyAvailable" })
      .build();
    const app = await renderApp(harness);

    // The proxied clip starts on its own.
    expect(await screen.findByLabelText("Pause")).toBeVisible();

    // Its neighbour has no proxy, so the same setting leaves it paused.
    await app.user.click(screen.getByLabelText("Next clip"));
    expect(await screen.findByLabelText("Play")).toBeVisible();
  });

  it("asks the app to check its media origin when an element fails", async () => {
    const harness = session();
    await renderApp(harness);
    harness.clearLog();

    await harness.media(first).failed("decode");

    await waitFor(() => expect(harness.calls("recover_media_origin")).toHaveLength(1));
    // The origin is healthy, so nothing on screen is invalidated: an ordinary
    // decode failure is the file's problem, not the transport's.
    expect(screen.getByLabelText("Play")).toBeVisible();
  });

  it("rebuilds every URL when the origin comes back on a new port", async () => {
    const harness = session();
    await renderApp(harness);
    harness.state.mediaOrigin = { baseUrl: "http://127.0.0.1:60000", token: "second-token", debugUrl: null };

    await harness.media(first).failed("origin");

    await waitFor(() =>
      expect(document.querySelector('video[data-cache-slot="current"]')?.getAttribute("src")).toBe(
        `http://127.0.0.1:60000/media/${first}?rep=proxy&t=second-token`,
      ),
    );
    expect(await screen.findByText(/Reconnected to the local media server/)).toBeVisible();
  });

  it("says so plainly when the media server never started", async () => {
    const harness = session();
    await renderApp(harness);
    harness.state.mediaOrigin = null;

    await harness.media(first).failed("origin");

    expect(await screen.findByText(/local media server did not start/)).toBeVisible();
  });

  it("shows a slate rather than a player when there is no origin at all", async () => {
    const harness = desktopScenario()
      .project("Hokkaido field test", { id: "project_hokkaido" })
      .camera("Sony A7C II", { clips: 2, withProxy: true })
      .withoutMediaOrigin()
      .build();
    await renderApp(harness);

    expect(document.querySelector("video")).toBeNull();
    const slate = stageSlate();
    expect(slate).not.toBeNull();
    expect(within(slate!).getByText("SONY_001.MP4")).toBeVisible();
  });
});
