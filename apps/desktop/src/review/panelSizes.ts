/**
 * User-draggable panel sizes.
 *
 * The design originally rejected resizable splitters — one user, one screen,
 * and muscle memory across hundreds of repetitions is worth more than fit.
 * The user overrode that, so sizes are draggable and persisted. What does NOT
 * become flexible is the notes field: dragging changes the queue and rail
 * widths and the ribbon height, and the rail absorbs its slack in the chip
 * well, so the notes field keeps the fixed height and fixed top offset that
 * rule 3 requires.
 */

export type PanelKind = "queue" | "rail" | "ribbon";

export interface PanelSizes {
  queue: number;
  rail: number;
  /** null means "size to the ribbon's own content". */
  ribbon: number | null;
}

export const DEFAULT_PANEL_SIZES: PanelSizes = { queue: 300, rail: 400, ribbon: null };

export const PANEL_LIMITS: Record<PanelKind, { min: number; max: number }> = {
  // The queue still has to fit a 72px thumbnail plus a readable title.
  queue: { min: 220, max: 520 },
  // Below ~320 the notes measure drops under ~40 characters, which is the
  // cramped state this whole overhaul was undoing.
  rail: { min: 320, max: 620 },
  ribbon: { min: 28, max: 520 },
};

/**
 * Clamps to the panel's own limits and to what the viewport can spare, so a
 * drag can never squeeze the video column out of existence or leave a panel
 * wider than the window.
 */
export function clampPanel(kind: PanelKind, value: number, viewport: number): number {
  const { min, max } = PANEL_LIMITS[kind];
  const ceiling = Math.min(max, Math.max(min, Math.round(viewport * 0.5)));
  if (!Number.isFinite(value)) return min;
  return Math.max(min, Math.min(ceiling, Math.round(value)));
}

export const PANEL_SIZES_KEY = "rcdvlog.panelSizes.v1";

interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** Anything unparseable falls back to the defaults rather than throwing. */
export function loadPanelSizes(storage: StorageLike | null | undefined): PanelSizes {
  if (!storage) return { ...DEFAULT_PANEL_SIZES };
  let raw: string | null = null;
  try {
    raw = storage.getItem(PANEL_SIZES_KEY);
  } catch {
    return { ...DEFAULT_PANEL_SIZES };
  }
  if (!raw) return { ...DEFAULT_PANEL_SIZES };

  try {
    const parsed = JSON.parse(raw) as Partial<PanelSizes>;
    return {
      queue: typeof parsed.queue === "number" ? parsed.queue : DEFAULT_PANEL_SIZES.queue,
      rail: typeof parsed.rail === "number" ? parsed.rail : DEFAULT_PANEL_SIZES.rail,
      ribbon: typeof parsed.ribbon === "number" ? parsed.ribbon : null,
    };
  } catch {
    return { ...DEFAULT_PANEL_SIZES };
  }
}

export function savePanelSizes(storage: StorageLike | null | undefined, sizes: PanelSizes): void {
  if (!storage) return;
  try {
    storage.setItem(PANEL_SIZES_KEY, JSON.stringify(sizes));
  } catch {
    // A full or disabled store is not worth interrupting a labelling session.
  }
}
