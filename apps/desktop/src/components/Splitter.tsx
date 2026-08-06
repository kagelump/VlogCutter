import { useCallback, useRef, type PointerEvent as ReactPointerEvent } from "react";

export interface SplitterProps {
  orientation: "vertical" | "horizontal";
  /** Current size of the panel this splitter resizes, in px. */
  value: number;
  /**
   * Which way the panel grows relative to pointer movement. The queue grows
   * as the pointer moves right (+1); the rail and the ribbon grow as it moves
   * left or up (-1).
   */
  direction: 1 | -1;
  label: string;
  onResize(next: number): void;
  onCommit(): void;
  /** Restores the size this panel has when nothing has been dragged. */
  onReset(): void;
}

const KEYBOARD_STEP = 16;

/**
 * A drag handle between panels.
 *
 * Uses pointer capture rather than window listeners so a fast drag that
 * outruns the cursor keeps delivering events to the handle, and gives the
 * separator arrow-key support — a 5px target is not something to require a
 * steady hand for.
 */
export function Splitter({
  orientation,
  value,
  direction,
  label,
  onResize,
  onCommit,
  onReset,
}: SplitterProps) {
  const origin = useRef<{ position: number; size: number } | null>(null);

  const handlePointerDown = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      event.preventDefault();
      event.currentTarget.setPointerCapture(event.pointerId);
      origin.current = {
        position: orientation === "vertical" ? event.clientX : event.clientY,
        size: value,
      };
    },
    [orientation, value],
  );

  const handlePointerMove = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      const start = origin.current;
      if (!start) return;
      const position = orientation === "vertical" ? event.clientX : event.clientY;
      onResize(start.size + (position - start.position) * direction);
    },
    [direction, onResize, orientation],
  );

  const endDrag = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (!origin.current) return;
      origin.current = null;
      if (event.currentTarget.hasPointerCapture(event.pointerId)) {
        event.currentTarget.releasePointerCapture(event.pointerId);
      }
      onCommit();
    },
    [onCommit],
  );

  return (
    <div
      className={`splitter splitter-${orientation}`}
      role="separator"
      aria-label={label}
      aria-orientation={orientation === "vertical" ? "vertical" : "horizontal"}
      aria-valuenow={Math.round(value)}
      tabIndex={0}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onDoubleClick={onReset}
      onKeyDown={(event) => {
        const decrease = orientation === "vertical" ? "ArrowLeft" : "ArrowUp";
        const increase = orientation === "vertical" ? "ArrowRight" : "ArrowDown";
        if (event.key !== decrease && event.key !== increase) return;
        event.preventDefault();
        onResize(value + (event.key === increase ? KEYBOARD_STEP : -KEYBOARD_STEP) * direction);
        onCommit();
      }}
    />
  );
}
