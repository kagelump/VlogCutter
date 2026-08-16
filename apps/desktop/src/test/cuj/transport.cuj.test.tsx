/**
 * The transport's own controls: skip, scrub, best-moment, mute, frame-only.
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

import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { desktopScenario, renderApp, selectedRow } from "../harness";

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

describe("the frame-only control", () => {
  it("enters Focus with nothing over the frame, rather than a second fullscreen", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });

    await app.user.click(screen.getByLabelText("Frame only"));

    // One fullscreen concept, and it is Focus. Native fullscreen was a second
    // one with its own state, its own exit, and no relation to the `F` this
    // same button used to advertise.
    await waitFor(() => expect(document.querySelector(".app-shell")).toHaveClass("is-focus"));
    expect(document.querySelector(".app-shell")).not.toHaveClass("show-overlays");
    expect(screen.getByLabelText("Frame only")).toHaveAttribute("aria-pressed", "true");
  });

  it("returns to Review when pressed again", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });
    await app.user.click(screen.getByLabelText("Frame only"));
    await waitFor(() => expect(document.querySelector(".app-shell")).toHaveClass("is-focus"));

    await app.user.click(screen.getByLabelText("Frame only"));

    await waitFor(() => expect(document.querySelector(".app-shell")).not.toHaveClass("is-focus"));
  });

  it("takes the overlays down when Focus was entered with F", async () => {
    const harness = session();
    const app = await renderApp(harness);
    await harness.media(first).loaded({ duration: 24 });
    await act(async () => { screen.getByLabelText("Notes").blur(); });
    await app.user.keyboard("f");
    await waitFor(() => expect(document.querySelector(".app-shell")).toHaveClass("is-focus"));
    expect(screen.getByLabelText("Frame only")).toHaveAttribute("aria-pressed", "false");

    await app.user.click(screen.getByLabelText("Frame only"));

    // Not an exit: the button means "just the frame", and from Focus-with-notes
    // the thing between the user and just the frame is the notes card.
    expect(document.querySelector(".app-shell")).toHaveClass("is-focus");
    expect(screen.getByLabelText("Frame only")).toHaveAttribute("aria-pressed", "true");
  });

  it("keeps the clip, the playhead and the notes across the round trip", async () => {
    const harness = session();
    const app = await renderApp(harness);
    const clip = harness.media(first);
    await clip.loaded({ duration: 24 });
    await app.user.type(screen.getByLabelText("Notes"), "the light changes here");
    await act(async () => { screen.getByLabelText("Notes").blur(); });
    await clip.advanceTo(12);

    await app.user.click(screen.getByLabelText("Frame only"));
    await waitFor(() => expect(document.querySelector(".app-shell")).toHaveClass("is-focus"));
    await app.user.click(screen.getByLabelText("Frame only"));

    await waitFor(() => expect(document.querySelector(".app-shell")).not.toHaveClass("is-focus"));
    expect(selectedRow()?.textContent).toContain("SONY_001.MP4");
    expect(screen.getByLabelText("Notes")).toHaveValue("the light changes here");
    expect(clip.element().currentTime).toBe(12);
  });
});
