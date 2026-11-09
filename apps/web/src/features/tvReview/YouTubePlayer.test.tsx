import { act, render, screen } from "@testing-library/react";
import { YouTubePlayer } from "./YouTubePlayer";

type Events = { onReady(): void; onStateChange(): void; onError(event: { data: number }): void };
let events: Events;
let state: number;
let destroy: ReturnType<typeof vi.fn>;
beforeEach(() => {
  state = -1;
  destroy = vi.fn();
  window.YT = { Player: class {
    constructor(_element: HTMLElement, options: Record<string, unknown>) { events = options.events as Events; }
    getCurrentTime() { return 12; }
    getPlayerState() { return state; }
    getPlaybackRate() { return 1; }
    pauseVideo() {}
    destroy() { destroy(); }
  } };
});
afterEach(() => { delete window.YT; });

test("YouTube API errors do not override the iframe and successful playback keeps sampling", async () => {
  const observation = vi.fn();
  await act(async () => { render(<YouTubePlayer videoId="synthetic01" recording onObservation={observation} />); });
  act(() => { events.onReady(); events.onError({ data: 5 }); });
  expect(screen.queryByRole("alert")).toBeNull();
  act(() => { state = 1; events.onStateChange(); });
  expect(screen.queryByRole("alert")).toBeNull();
  expect(observation).toHaveBeenCalledWith(12, 1, 1, expect.any(Number));
  act(() => { events.onError({ data: 5 }); });
  expect(screen.queryByRole("alert")).toBeNull();
});

test("the iframe owns private-video errors and a new video resets readiness", async () => {
  const callback = vi.fn();
  const view = render(<YouTubePlayer videoId="synthetic01" recording={false} onObservation={callback} />);
  await act(async () => {});
  act(() => { events.onReady(); events.onError({ data: 150 }); });
  expect(screen.queryByRole("alert")).toBeNull();
  view.rerender(<YouTubePlayer videoId="synthetic02" recording={false} onObservation={callback} />);
  await act(async () => {});
  expect(destroy).toHaveBeenCalledOnce();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.getByText("Loading YouTube player…")).toBeVisible();
  expect(callback).not.toHaveBeenCalled();
});
