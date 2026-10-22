# Independent tester fixtures — D01-T007

Owned by the independent tester for D01-T007 ("Chronology domain model and
operation vocabulary"). These fixtures are built by
`tests/chronology_migration_independent.rs` alone and are intentionally
unrelated to the implementer's fixtures under `tests/fixtures/chronology/`.

## Construction method (genuinely old catalogs)

Every `*.sqlite3` file in this directory is a schema-8 SQLite catalog
produced by:

1. running the real production migration runner for migrations 1..=8 only
   (`run_sqlite_migrations(conn, &PROJECT_CATALOG_MIGRATIONS[..8])`), then
2. inserting `projects`, `assets`, and `clock_events` rows directly, and
3. computing the old-model projections (`assets.best_time`,
   `assets.offset_seconds`) with the same rule the pre-migration production
   code used (`recalculate_clock` in `src/operations.rs`): per camera, the
   offset of the last clock event whose `start_filename <= asset.filename`,
   and `best_time = normalize_datetime(captured_at_raw, offset)`.

These are genuine old catalogs, not new catalogs described as old. The tests
copy the committed file into a tempdir and migrate the copy, so the committed
artifacts are never mutated.

Regenerate with:

```sh
cargo test -p rcdvlog-core --test chronology_migration_independent rebuild_committed_fixture_catalogs -- --ignored
```

If a committed catalog is missing, the tests fall back to building it at
runtime in a tempdir through the same construction method.

## Catalog manifest

| Catalog | Assets | Cameras | Clock events | What it attacks |
| --- | --- | --- | --- | --- |
| `one_camera_no_clock_events` | 4 | 1 | 0 | acceptance oracle shape 1 |
| `one_camera_one_clock_event` | 4 | 1 | 1 | acceptance oracle shape 2 |
| `two_cameras_one_clock_event_each` | 6 | 2 | 2 | acceptance oracle shape 3 |
| `two_cameras_three_events_one_camera` | 12 | 2 | 4 | acceptance oracle shape 4 (three overlapping events on one camera) |
| `no_digit_run_filenames` | 3 | 1 | 1 | acceptance oracle shape 5 (filenames without digit runs) |
| `overlapping_counter_ranges` | 20 | 2 | 2 | two cameras sharing the same filename counter range |
| `clock_event_start_filename_missing` | 3 | 1 | 1 | clock event names a `start_filename` no asset has |
| `null_captured_at` | 3 | 1 | 1 | asset with NULL `captured_at_raw` (NULL best_time sorts last) |
| `identical_raw_and_filename_different_folders` | 3 | 1 | 1 | identical `captured_at_raw` AND identical filenames in different folders (id tiebreak) |
| `non_ascii_prefix_digit_run` | 4 | 1 | 1 | non-ASCII filename prefix with a digit run; best_time tie broken by non-ASCII filename |
| `huge_counter_filename` | 2 | 1 | 1 | digit run exceeding u64 must yield NULL, never panic |
| `counter_parsing_examples` | 6 | 1 | 0 | the frozen documented parsing examples (C0101.MP4 -> 101 etc.) |
| `large_three_camera_500` | 500 | 3 | 3 | 500 assets across three cameras with overlapping counters and interleaved times |

Filename counters for newly identified footage give an explicit camera pattern
priority over the generic trailing-digit fallback. For example,
`DJI_20251221092058_0001_D.MP4` uses the DJI pattern's counter `1`; its trailing
14-digit timestamp is not a counter. The `counter_parsing_examples` fixture
freezes the generic trailing-digit cases, while the pattern table examples and
unit tests in `core/rcdvlog-core/src/cameras/` freeze this precedence rule.

Camera repair prefers a parseable filename counter over the stored card-derived
counter. To exercise the card fallback, use synthetic filenames without
parseable trailing digits or a recognized camera pattern; a name such as
`clip_0001.mp4` would exercise filename precedence instead.

## Media file

`media/sample.mp4` is a tiny synthetic file (deterministic bytes, not a real
video). The media-immutability test points an asset's `path` at it and hashes
it before and after a full migration plus operation-application cycle.
