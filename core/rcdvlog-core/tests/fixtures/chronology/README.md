# Chronology fixture catalogs (D01-T007)

These are genuine **schema-8** project catalogs: each was built by running the
first eight catalog migrations on an empty file and inserting rows directly —
not by creating a schema-9 catalog and removing migration 9's history. They
are the implementer's fixtures; the independent tester builds their own under
`tests/fixtures/chronology-independent/`.

| File | Project id | Shape |
| --- | --- | --- |
| `one_camera_no_clock.sqlite3` | `project_fixture_solo` | One camera ("DJI Pocket 3"), 4 assets, no clock events. Two assets share one `best_time` to exercise the filename/id tie-break. |
| `one_camera_one_clock.sqlite3` | `project_fixture_single` | One camera ("Sony A7C II"), 3 assets, one clock event at `C0102.MP4` (-3600 s). |
| `two_cameras_one_clock_each.sqlite3` | `project_fixture_two` | Two cameras ("GoPro Hero 11", "iPhone 15 Pro"), 2 assets each, one clock event each. |
| `two_cameras_three_clock_events.sqlite3` | `project_fixture_three` | Camera "Insta360 X4" with 6 assets and three overlapping clock events at `C0001`/`C0003`/`C0005`; camera "DJI Mini 4 Pro" with 2 assets and no events. |
| `no_digit_run_filenames.sqlite3` | `project_fixture_no_counters` | One camera ("Canon R6"), filenames with no digit run, one clock event that therefore applies to the whole camera (`start_counter` 0). |
| `media/C0001.MP4` | — | 2048 deterministic bytes. Referenced by the source-media invariant test; no code path reads it during migration or operation application. |

Regenerate all fixtures (including the media file) with:

```sh
cargo test -p rcdvlog-core --test chronology_migration regenerate_fixture_catalogs -- --ignored
```

The builder is deterministic apart from `created_at` timestamps, which no
oracle compares.
