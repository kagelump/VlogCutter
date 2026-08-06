import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { Splitter } from "../Splitter";

function renderSplitter(direction: 1 | -1 = 1) {
  const onResize = vi.fn();
  const onCommit = vi.fn();
  const onReset = vi.fn();
  const view = render(
    <Splitter
      orientation="vertical"
      value={300}
      direction={direction}
      label="Resize test panel"
      onResize={onResize}
      onCommit={onCommit}
      onReset={onReset}
    />,
  );
  const splitter = view.getByRole("separator");
  Object.defineProperties(splitter, {
    setPointerCapture: { value: vi.fn() },
    hasPointerCapture: { value: vi.fn(() => true) },
    releasePointerCapture: { value: vi.fn() },
  });
  return { splitter, onResize, onCommit, onReset };
}

describe("Splitter", () => {
  it("resizes continuously from the pointer-down origin and commits on release", () => {
    const { splitter, onResize, onCommit } = renderSplitter();
    fireEvent.pointerDown(splitter, { pointerId: 1, clientX: 300 });
    fireEvent.pointerMove(splitter, { pointerId: 1, clientX: 348 });
    fireEvent.pointerUp(splitter, { pointerId: 1, clientX: 348 });
    expect(onResize).toHaveBeenCalledWith(348);
    expect(onCommit).toHaveBeenCalledTimes(1);
  });

  it("reverses the delta for a panel attached to the far edge", () => {
    const { splitter, onResize } = renderSplitter(-1);
    fireEvent.pointerDown(splitter, { pointerId: 1, clientX: 900 });
    fireEvent.pointerMove(splitter, { pointerId: 1, clientX: 852 });
    expect(onResize).toHaveBeenCalledWith(348);
  });

  it("supports arrow-key resizing and double-click reset", () => {
    const { splitter, onResize, onCommit, onReset } = renderSplitter();
    fireEvent.keyDown(splitter, { key: "ArrowRight" });
    expect(onResize).toHaveBeenCalledWith(316);
    expect(onCommit).toHaveBeenCalledTimes(1);
    fireEvent.doubleClick(splitter);
    expect(onReset).toHaveBeenCalledTimes(1);
  });
});
