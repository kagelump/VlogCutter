/**
 * The transport's own controls: skip, scrub, best-moment, mute, fullscreen.
 *
 * `playback.cuj.test.tsx` covers what the app does when the *element* reports
 * something. This covers the other direction — what the app does to the element
 * when the user presses a control — which left `seekTo`, `seekBy` and
 * `toggleMute` in `App.tsx` and five of `TransportBar`'s handlers unreached.
 *
 * The logic worth having here is the clamping. `seekBy` bounds its result to
 * `[0, duration]`, and both ends are ordinary user actions: pressing −15 near
 * the start of a clip, and +15 near the end. An unclamped seek sets
 * `currentTime` out of range, which a real element rejects or silently
 * normalises — a defect that would never show up in jsdom as a crash, only as a
 * position that disagrees with the element.
 *
 * As everywhere in this tier: the element is real and the DOM events are real,
 * but nothing decodes. That an unbuffered seek actually paints stays a native
 * acceptance criterion.
 */

import { fireEvent, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, renderApp } from "../harness";

const first = "asset_sony_a7c_ii_001";

function session() {
  return desktopScenario({ now: "2026-08-09T12:00:00.000Z" })
    .project("Hokkaido field test", { id: "project_hokkaido" })
    .camera("Sony A7C II", { clips: 2, withProxy: true, durationSeconds: 24 })
    .settings({ autoplayMode: "never" })
    .build();
}

function transportTime(): string {
  return document.querySelector(".transport-time")?.textContent?.trim() ?? "";
}

describe("skipping within a clip", () => {
  it("moves the element forward by fifteen seconds and follows it", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });
    await harness.media(first).advanceTo(5);

    await app.user.click(screen.getByLabelText("Forward 15 seconds"));

    // The element is the source of truth; the readout is the app agreeing.
    expect(harness.media(first).element().currentTime).toBe(20);
    await waitFor(() => expect(transportTime()).toBe("0:20 / 0:24"));
  });

  it("skips back without ever going negative", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });
    await harness.media(first).advanceTo(4);

    await app.user.click(screen.getByLabelText("Back 15 seconds"));

    // 4 − 15 is −11. A negative currentTime is not a position.
    expect(harness.media(first).element().currentTime).toBe(0);
    await waitFor(() => expect(transportTime()).toBe("0:00 / 0:24"));
  });

  it("stops at the end rather than seeking past it", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });
    await harness.media(first).advanceTo(20);

    await app.user.click(screen.getByLabelText("Forward 15 seconds"));

    expect(harness.media(first).element().currentTime).toBe(24);
    await waitFor(() => expect(transportTime()).toBe("0:24 / 0:24"));
  });
});

describe("scrubbing", () => {
  it("seeks the element to the position the slider was dragged to", async () => {
    const harness = session();
    await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });

    fireEvent.change(screen.getByLabelText("Video position"), { target: { value: "9.5" } });

    expect(harness.media(first).element().currentTime).toBe(9.5);
    await waitFor(() => expect(transportTime()).toBe("0:09 / 0:24"));
  });
});

describe("muting", () => {
  it("starts muted, which is what lets autoplay begin at all", async () => {
    const harness = session();
    await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });

    // Browser autoplay policy permits a muted start and blocks an audible one,
    // so this default is load-bearing rather than a preference.
    expect(harness.media(first).element().muted).toBe(true);
    expect(screen.getByLabelText("Unmute")).toHaveAttribute("aria-pressed", "true");
  });

  it("unmutes the element the user is watching", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });

    await app.user.click(screen.getByLabelText("Unmute"));

    expect(harness.media(first).element().muted).toBe(false);
    expect(await screen.findByLabelText("Mute")).toHaveAttribute("aria-pressed", "false");
  });

  it("mutes again on a second press", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });

    await app.user.click(screen.getByLabelText("Unmute"));
    await app.user.click(await screen.findByLabelText("Mute"));

    expect(harness.media(first).element().muted).toBe(true);
    expect(await screen.findByLabelText("Unmute")).toHaveAttribute("aria-pressed", "true");
  });

  it("carries the choice onto the next clip rather than resetting it", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });
    await app.user.click(screen.getByLabelText("Unmute"));

    await app.user.click(screen.getByLabelText("Next clip"));
    const second = harness.media("asset_sony_a7c_ii_002");
    await second.loaded({ duration: 24 });

    // Re-muting every clip would make the control useless during a review pass.
    await waitFor(() => expect(second.element().muted).toBe(false));
  });
});

describe("fullscreen", () => {
  it("says so rather than failing silently when the platform refuses", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });

    await app.user.click(screen.getByLabelText("Fullscreen"));

    // jsdom implements no Fullscreen API, which is exactly the shape of a
    // platform that refuses: the user gets told, and the app stays usable.
    expect(await screen.findByRole("alert")).toHaveTextContent(/Fullscreen unavailable/);
  });
});
