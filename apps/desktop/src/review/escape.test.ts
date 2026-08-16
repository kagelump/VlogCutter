import { describe, expect, it } from "vitest";
import { resolveEscape, type EscapeLayer } from "./escape";

const stack = (...layers: EscapeLayer[]) => layers;

describe("resolveEscape", () => {
  it("does nothing when no surface is open", () => {
    expect(resolveEscape(stack({ id: "settings", open: false }, { id: "focus", open: false })))
      .toEqual({ kind: "none" });
  });

  it("closes the only open surface", () => {
    expect(resolveEscape(stack({ id: "settings", open: true }, { id: "focus", open: false })))
      .toEqual({ kind: "close", id: "settings" });
  });

  it("closes the topmost surface and leaves the rest alone", () => {
    // The bug this replaces: a handler that closed four surfaces at once, so
    // one Escape dismissed a dialog and exited Focus underneath it.
    const resolution = resolveEscape(stack(
      { id: "pairing", open: true },
      { id: "settings", open: true },
      { id: "focus", open: true },
    ));
    expect(resolution).toEqual({ kind: "close", id: "pairing" });
  });

  it("lets a locked surface swallow the key rather than passing it down", () => {
    // A running import must not be cancelled by a keystroke — and must not
    // let the keystroke reach Focus instead, which is the same defect wearing
    // a different outcome.
    expect(resolveEscape(stack(
      { id: "import", open: true, locked: true },
      { id: "focus", open: true },
    ))).toEqual({ kind: "blocked", id: "import" });
  });

  it("ignores a locked surface that is not open", () => {
    expect(resolveEscape(stack(
      { id: "import", open: false, locked: true },
      { id: "focus", open: true },
    ))).toEqual({ kind: "close", id: "focus" });
  });

  it("reaches a lower surface only when everything above it is closed", () => {
    expect(resolveEscape(stack(
      { id: "import", open: false },
      { id: "settings", open: false },
      { id: "mode", open: true },
    ))).toEqual({ kind: "close", id: "mode" });
  });
});
