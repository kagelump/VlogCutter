export interface CustomPropertyUsage {
  defined: Set<string>;
  referenced: Array<{ name: string; hasFallback: boolean }>;
}

/**
 * Scans CSS text for custom-property definitions and references.
 *
 * A reference without a definition and without a fallback is invalid at
 * computed-value time: the declaration falls back to the property's initial
 * value, which silently removes borders and resets colours rather than
 * failing loudly. This exists so a test can forbid that.
 */
export function collectCustomProperties(css: string): CustomPropertyUsage {
  const defined = new Set<string>();
  for (const match of css.matchAll(/(--[\w-]+)\s*:/g)) {
    defined.add(match[1]);
  }

  const referenced: Array<{ name: string; hasFallback: boolean }> = [];
  for (const match of css.matchAll(/var\(\s*(--[\w-]+)\s*(,)?/g)) {
    referenced.push({ name: match[1], hasFallback: Boolean(match[2]) });
  }

  return { defined, referenced };
}

/**
 * Custom properties a source file assigns through a React style object, as in
 * `style={{ "--clip-step": "68px" }}`.
 *
 * These never appear in the stylesheet but are perfectly well defined at
 * runtime, so a stylesheet-only audit would report them as broken. Collecting
 * them keeps the audit's failures real.
 */
export function collectStyleObjectProperties(source: string): Set<string> {
  const assigned = new Set<string>();
  for (const match of source.matchAll(/["'](--[\w-]+)["']\s*:/g)) {
    assigned.add(match[1]);
  }
  return assigned;
}
