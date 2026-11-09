import { act, render, screen } from "@testing-library/react";
import { YouTubePlayer } from "./YouTubePlayer";
import { createRef } from "react";
import type { YouTubePlayerHandle } from "./YouTubePlayer";

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

test("an already-playing video is sampled when capture becomes ready, periodically and before stop", async () => {
  vi.useFakeTimers();
  try {
    state = 1;
    const observation = vi.fn();
    const ref = createRef<YouTubePlayerHandle>();
    const view = render(<YouTubePlayer ref={ref} videoId="synthetic01" recording={false} captureReady={false} onObservation={observation} />);
    await act(async () => {});
    act(() => events.onReady());
    view.rerender(<YouTubePlayer ref={ref} videoId="synthetic01" recording captureReady={false} onObservation={observation} />);
    expect(observation).not.toHaveBeenCalled();
    view.rerender(<YouTubePlayer ref={ref} videoId="synthetic01" recording captureReady onObservation={observation} />);
    expect(observation).toHaveBeenCalledOnce();
    act(() => vi.advanceTimersByTime(5000));
    expect(observation).toHaveBeenCalledTimes(2);
    act(() => ref.current?.sample());
    expect(observation).toHaveBeenCalledTimes(3);
    view.rerender(<YouTubePlayer ref={ref} videoId="synthetic01" recording={false} captureReady onObservation={observation} />);
    act(() => vi.advanceTimersByTime(5000));
    expect(observation).toHaveBeenCalledTimes(3);
    view.unmount();
  } finally { vi.useRealTimers(); }
});
