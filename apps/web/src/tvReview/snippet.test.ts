import { snippetWindows, utteranceWindow } from "./snippet";

test("windows are bounded and never cross a gap", () => {
  const windows = snippetWindows([{ startFrame: 0, endFrame: 70 }, { startFrame: 100, endFrame: 110 }], 1, 30);
  expect(windows).toEqual([
    { startFrame: 0, endFrame: 30 }, { startFrame: 30, endFrame: 60 }, { startFrame: 60, endFrame: 70 },
    { startFrame: 100, endFrame: 110 },
  ]);
});

test("an utterance window adds context but stays inside its stored range and the bound", () => {
  const ranges = [{ startFrame: 0, endFrame: 480_000 }, { startFrame: 960_000, endFrame: 3_000_000 }];
  expect(utteranceWindow(ranges, 48_000, 48_000, 96_000)).toEqual({ startFrame: 24_000, endFrame: 120_000 });
  // Clipped at the range start and end; never reaching into the gap.
  expect(utteranceWindow(ranges, 48_000, 0, 470_000)).toEqual({ startFrame: 0, endFrame: 480_000 });
  // Longer than the bound: truncated at 30 s from the window start.
  expect(utteranceWindow(ranges, 48_000, 960_000, 2_900_000)).toEqual({ startFrame: 960_000, endFrame: 960_000 + 30 * 48_000 });
  // Starts in the gap: nothing stored to play.
  expect(utteranceWindow(ranges, 48_000, 500_000, 600_000)).toBeNull();
});
