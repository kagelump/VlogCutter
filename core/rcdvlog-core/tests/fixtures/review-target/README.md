# Synthetic TV review target v1

`v1.json` is authored from invented IDs, clocks and ranges. No real catalog,
movie, filename, URL, transcript or audio contributed to it. The movie hash,
plan hash, constraint hash and byte length are **fictional**; there is no movie
to verify. Its target digest is real BLAKE3 over the typed synthetic snapshot.
The fixed verification date allows deterministic freshness tests. It does not
prove local media availability, trusted provenance or current freshness.

The three cuts all use `asset_repeated`: two adjacent split ranges, followed
by a repeated range. The second owner is explicitly silent. The third owns
`asset_sound_only` placed at elapsed 1..4 seconds, independently of its picture
at 4..6 seconds. Legacy linked sound on the first owner may overlap that sound.
The nonzero movie origin is 90000 ticks at 1/90000 seconds. Output cuts occupy
elapsed [0,2), [2,4), [4,6); duration 6.02 leaves a container/audio tail with no
picture candidate. These values exercise interface semantics, not precision
of a real renderer, room recording or soundtrack matcher.

Use this fixture for web selection, preflight, candidate-cut displays and
negative mutations. Every session must retain its immutable target snapshot.
See [the versioned contract](../../../../../docs/tv-review-handoff.md).
