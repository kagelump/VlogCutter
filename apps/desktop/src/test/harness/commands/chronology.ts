/**
 * The chronology read surface.
 *
 * The overview is whatever the scenario declared. Nothing here derives an
 * order, a contradiction, or an uncertainty: `rcdvlog-core` owns the solver, and
 * a second one in TypeScript could disagree with it while looking convincing.
 *
 * Editing commands and their declared outcomes arrive with H01-T005.
 */

import type { ChronologyOverview } from "../../../chronologyTypes";
import type { CommandTable } from "../registry";
import { requireString } from "../registry";

export const chronologyCommands: CommandTable = {
  chronology_overview: (args, { state }): ChronologyOverview => {
    requireString(args, "projectId");
    return state.chronology.overview;
  },

  explain_chronology: (args, { state }) => state.chronology.explanations[requireString(args, "assetId")] ?? null,
};
