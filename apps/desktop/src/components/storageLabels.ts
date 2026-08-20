/**
 * What each cache is, what it costs to make again, and what stops working
 * until it is (#16).
 *
 * Copy lives here rather than in the core because the core's job is to measure
 * bytes, not to explain them — and because the same explanation is needed in
 * two places (the storage panel and the delete confirmation) and must not drift
 * between them.
 *
 * Every line answers the question a user actually has before clicking Clear:
 * *what will I lose, and what will it cost me to get it back?*
 */

import type { CacheKind } from "../types";

export interface CacheKindLabel {
  title: string;
  /** How it comes back, and what it needs to come back. */
  regeneration: string;
  /** What does not work until it does. */
  unavailable: string;
  /** True when regenerating needs the original media reachable. */
  needsOriginals: boolean;
}

export const CACHE_KIND_LABELS: Record<CacheKind, CacheKindLabel> = {
  proxies: {
    title: "Review proxies",
    regeneration: "Remade from the originals, so the drive or card has to be connected again.",
    unavailable: "Clips play from their originals until the proxies are remade; offline clips cannot be reviewed at all.",
    needsOriginals: true,
  },
  thumbnails: {
    title: "Queue and ribbon posters",
    regeneration: "Remade from a proxy, or from the original when there is no proxy.",
    unavailable: "Rows and ribbon tiles fall back to filenames until they are remade.",
    needsOriginals: false,
  },
  audio: {
    title: "Extracted audio",
    regeneration: "Re-extracted from the originals when speech analysis next runs.",
    unavailable: "Nothing, until you run speech analysis again.",
    needsOriginals: true,
  },
  framePacks: {
    title: "Analysis frames",
    regeneration: "Re-decoded from the proxies when vision analysis next runs.",
    unavailable: "Nothing. Analysis results already saved are kept — this is only the input.",
    needsOriginals: false,
  },
  editPreviews: {
    title: "Edit-plan previews",
    regeneration: "Re-rendered on demand when you preview a plan.",
    unavailable: "The next preview takes as long as the first one did.",
    needsOriginals: false,
  },
  importPreviews: {
    title: "Import dialog posters",
    regeneration: "Re-decoded from the card the next time you open Smart ingest.",
    unavailable: "Nothing. The import dialog fills them in again as you scroll.",
    needsOriginals: false,
  },
  analysisModels: {
    title: "Downloaded analysis models",
    regeneration: "Downloaded again — gigabytes over the network, not seconds of ffmpeg.",
    unavailable: "Local speech analysis cannot run until they are downloaded again.",
    needsOriginals: false,
  },
};

/** The order the storage panel lists categories in: costliest to replace last. */
export const CACHE_KIND_ORDER: CacheKind[] = [
  "proxies",
  "thumbnails",
  "audio",
  "framePacks",
  "editPreviews",
  "importPreviews",
  "analysisModels",
];
