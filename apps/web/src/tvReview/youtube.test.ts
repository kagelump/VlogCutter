import { parseYoutubeVideoId, unwrapHandoff } from "./youtube";

test.each([
  ["dQw4w9WgXcQ", "dQw4w9WgXcQ"],
  ["https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42s", "dQw4w9WgXcQ"],
  ["youtube.com/watch?v=dQw4w9WgXcQ", "dQw4w9WgXcQ"],
  ["https://youtu.be/dQw4w9WgXcQ?si=abc", "dQw4w9WgXcQ"],
  ["https://m.youtube.com/watch?v=dQw4w9WgXcQ", "dQw4w9WgXcQ"],
  ["https://www.youtube.com/shorts/dQw4w9WgXcQ", "dQw4w9WgXcQ"],
  ["https://www.youtube.com/live/dQw4w9WgXcQ?feature=share", "dQw4w9WgXcQ"],
  ["https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ", "dQw4w9WgXcQ"],
])("parses %s", (input, expected) => {
  expect(parseYoutubeVideoId(input)).toBe(expected);
});

test.each([
  "",
  "https://evil.example/watch?v=dQw4w9WgXcQ",
  "https://www.youtube.com/watch?v=short",
  "https://www.youtube.com/playlist?list=PL123",
  "javascript:alert(1)",
  "dQw4w9WgXcQ!",
  "https://youtu.be.evil.example/dQw4w9WgXcQ",
])("rejects %s", (input) => {
  expect(parseYoutubeVideoId(input)).toBeNull();
});

test("unwraps the CLI success envelope but passes a bare handoff through", () => {
  const data = { schemaVersion: 1 };
  expect(unwrapHandoff({ ok: true, command: "edit-plan.review-target-export", data })).toBe(data);
  expect(unwrapHandoff(data)).toBe(data);
  expect(unwrapHandoff([data])).toBeNull();
  expect(unwrapHandoff("text")).toBeNull();
});
