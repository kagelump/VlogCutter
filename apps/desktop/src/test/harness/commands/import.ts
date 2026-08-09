/**
 * Card detection and archive roots.
 *
 * `detect_mounted_cards` is polled from the moment a project is open, so it is
 * part of booting even in a scenario with no card in the slot. Preflight and the
 * import run itself arrive with H01-T007.
 */

import type { CardCandidate } from "../../../types";
import type { CommandTable } from "../registry";
import { requireString } from "../registry";

export const importCommands: CommandTable = {
  detect_mounted_cards: (_args, { state }): CardCandidate[] => state.cards,

  inspect_import_source: (args, { state }) => {
    const path = requireString(args, "path");
    return state.cards.find((card) => card.mountPath === path) ?? null;
  },

  list_archive_roots: (_args, { state }) => state.archiveRoots,
};
