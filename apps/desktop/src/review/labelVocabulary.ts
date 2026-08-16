import type { Asset } from "../types";

/**
 * The labels this project already uses (issue #11).
 *
 * Over a few thousand clips a free-text field fragments: `night walk`,
 * `nightwalk`, and `Night Walk` become three labels that no filter reconciles,
 * and the vocabulary the user is building quietly stops working. The fix is to
 * *show* what exists at the moment of typing, not to rewrite anything —
 * labels are user data, and nothing here renames, merges, or deletes one.
 *
 * Spellings that differ only by case are one entry, because that difference is
 * a typo rather than a distinction; the spelling offered is the one the
 * project uses most, and every other spelling stays exactly as the user wrote
 * it on the clips that carry it.
 */

export interface LabelUse {
  /** The spelling to offer: the most-used one, ties broken by first appearance. */
  label: string;
  /** How many clips carry this label in any casing. */
  count: number;
}

/**
 * Every label in use, most-used first. Ordering is by count and then
 * alphabetically, so the list is stable across reads of the same project
 * rather than following the queue's order.
 */
export function projectLabelVocabulary(assets: readonly Asset[]): LabelUse[] {
  const groups = new Map<string, { count: number; spellings: Map<string, number>; first: number }>();
  let seen = 0;
  for (const asset of assets) {
    for (const label of asset.labels) {
      const trimmed = label.trim();
      if (!trimmed) continue;
      const key = trimmed.toLocaleLowerCase();
      const group = groups.get(key) ?? { count: 0, spellings: new Map(), first: seen++ };
      group.count += 1;
      group.spellings.set(trimmed, (group.spellings.get(trimmed) ?? 0) + 1);
      groups.set(key, group);
    }
  }

  return [...groups.values()]
    .map((group) => ({ label: dominantSpelling(group.spellings), count: group.count }))
    .sort((a, b) => b.count - a.count || a.label.localeCompare(b.label));
}

function dominantSpelling(spellings: Map<string, number>): string {
  let best = "";
  let bestCount = -1;
  for (const [spelling, count] of spellings) {
    if (count > bestCount) {
      best = spelling;
      bestCount = count;
    }
  }
  return best;
}

/**
 * What to offer for what has been typed so far.
 *
 * Prefix matches come before matches found further in, because that is the
 * order the typist is thinking in. Labels already on this clip are left out:
 * offering one would be offering a no-op.
 */
export function suggestLabels(
  vocabulary: readonly LabelUse[],
  query: string,
  { exclude = [], limit = 6 }: { exclude?: readonly string[]; limit?: number } = {},
): LabelUse[] {
  const typed = query.trim().toLocaleLowerCase();
  const taken = new Set(exclude.map((label) => label.trim().toLocaleLowerCase()));
  const matches = vocabulary.filter((use) => {
    const label = use.label.toLocaleLowerCase();
    if (taken.has(label)) return false;
    return typed === "" || label.includes(typed);
  });

  if (typed !== "") {
    matches.sort((a, b) => {
      const rank = Number(b.label.toLocaleLowerCase().startsWith(typed))
        - Number(a.label.toLocaleLowerCase().startsWith(typed));
      return rank || b.count - a.count || a.label.localeCompare(b.label);
    });
  }
  return matches.slice(0, limit);
}

/**
 * The spelling to actually write when the user commits `value`.
 *
 * If the project already knows this label in some casing, that spelling wins:
 * typing `Market` where `market` exists should add the clip to the label that
 * exists, not open a second one beside it. Anything the project has not seen
 * is written exactly as typed — creating a new label is always allowed.
 */
export function canonicalLabel(vocabulary: readonly LabelUse[], value: string): string {
  const typed = value.trim();
  const key = typed.toLocaleLowerCase();
  return vocabulary.find((use) => use.label.toLocaleLowerCase() === key)?.label ?? typed;
}
