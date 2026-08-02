# D01-T012: Trip phases and timezone context

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T012",
  "epic_id": "D01",
  "title": "Trip phases and timezone context",
  "slug": "trip-phases-and-timezone-context",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["D01-T008"],
  "principal_invariant": "An asset's local display time derives from the trip phase covering its trip time, never from camera metadata timezone.",
  "central_falsification": "Build a trip crossing the date line with location and transit phases, and confirm each clip's displayed local time matches the phase timezone rather than the camera's recorded timezone.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": ["trip phase timezone -> local display time"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "independent_tester": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "reviewer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "integrator": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T012-trip-phases-and-timezone-context.md"],
    "implementer": [
      "core/rcdvlog-core/src/chronology/phases.rs",
      "core/rcdvlog-core/tests/chronology_phases.rs"
    ],
    "independent_tester": ["core/rcdvlog-core/tests/chronology_phases_independent.rs"],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T012/**"]
  },
  "forbidden_paths": ["apps/**", "packages/**", "core/rcdvlog-server/**", "core/rcdvlog-core/src/migrations.rs", ".github/**"],
  "verification_commands": [
    "cargo test -p rcdvlog-core --test chronology_phases",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T012\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Persist trip context — where the user physically was, and in which timezone — and
derive local display time from it, so a trip that crosses timezones displays
sensible local times instead of whatever the camera's clock happened to say.

This is the capability the product owner named first: persisted trip phases and
timezone context. Nothing in the repository stores trip context today.

## Principal invariant

This task is correct when an asset's local display time derives from the trip
phase covering its trip time, never from camera metadata timezone.

## Inputs and outputs

Inputs:

- The frozen `TripPhase`, `PhaseKind`, and `DisplayMode` types and the
  `tripPhaseSet` / `tripPhaseRemoved` operations.
- Solved trip times from D01-T008.
- `chrono-tz`, already a workspace dependency, for IANA timezone resolution.

Outputs:

- Phase resolution: which phase covers a trip time, and what that implies for
  display.
- `DisplayTime` population in the solution.
- `ProjectCatalog` methods to add, update, and remove a phase.

## Frozen decisions

- Timezones are IANA identifiers resolved through `chrono-tz`. No fixed-offset
  storage: a fixed offset is wrong across a daylight-saving boundary, and trips
  routinely cross one.
- Phase boundaries may be rough (`PRD.md` §10.3). Overlap and gaps are expected
  and must be handled, not rejected.
- Coverage rule, total and deterministic: the phase whose interval contains the
  trip time wins; on overlap the phase with the later `start_trip_time` wins, and
  on an exact tie the later HLC time wins; when no phase covers it,
  `display_mode` is `Utc` and `phase_id` is null. An uncovered clip is displayed
  honestly as UTC rather than guessed at.
- `DisplayMode::Dual` populates `DisplayTime::secondary`. For a transit phase,
  `primary` uses `origin_timezone` and `secondary` uses `destination_timezone`, so
  the user reads departure-local first — the order matters and is fixed here.
- Camera metadata timezone is retained as evidence and never used for display.
- Removing a phase never deletes or retimes an asset. Assets it covered become
  uncovered.
- A phase carries no assets and no membership list. Coverage is computed from trip
  time, so a chronology correction that moves a clip moves it between phases
  automatically.

## Owned paths

The implementer owns the phases module and its tests. This packet depends on
D01-T008 but not on the epoch or exception packets, so it can proceed in parallel
with them.

## Required interface

```rust
/// The phase covering this trip time by the coverage rule, if any.
pub fn covering_phase<'a>(phases: &'a [TripPhase], trip_time: &str) -> Option<&'a TripPhase>;

/// The display time for a trip time under a phase. `None` trip time yields `None`.
pub fn display_time(phases: &[TripPhase], trip_time: Option<&str>) -> Option<DisplayTime>;

impl ProjectCatalog {
    pub fn set_trip_phase(
        &self,
        project_id: &str,
        phase: &TripPhase,
        expected_revision: Option<u64>,
    ) -> Result<TripPhase>;

    pub fn remove_trip_phase(&self, phase_id: &str, expected_revision: u64) -> Result<()>;

    pub fn trip_phases(&self, project_id: &str) -> Result<Vec<TripPhase>>;
}
```

`trip_phases` returns phases sorted by `start_trip_time` then `id`, so callers
cannot inherit nondeterminism.

## Acceptance oracle

1. A location phase with timezone `Asia/Tokyo` covering a clip whose trip time is
   `2026-01-15T03:00:00Z` displays `2026-01-15T12:00:00+09:00`.
2. The same clip displays Tokyo local time even when its camera metadata timezone
   says `America/Toronto`. This is the invariant.
3. A transit phase from `Asia/Tokyo` to `America/Toronto` with `display_mode`
   `dual` populates `primary` in Tokyo time and `secondary` in Toronto time.
4. A date-line-crossing trip — Tokyo to Toronto — displays each clip in the
   timezone of the phase covering it, with the calendar date differing between
   primary and secondary for clips in the transit phase.
5. A daylight-saving transition inside a phase: two clips an hour apart across the
   boundary display offsets that differ by an hour, resolved by `chrono-tz` rather
   than by a stored offset.
6. A clip covered by no phase gets `display_mode` `Utc` and a null `phase_id`.
7. Overlapping phases: the later-starting phase wins, deterministically, across 32
   shuffles of the phase list.
8. A phase with a null timezone falls back to UTC display and records that in
   evidence rather than failing.
9. Removing a phase leaves every asset's trip time unchanged and only changes
   display.
10. An invalid IANA identifier is rejected with a stable error code before any
    write.
11. `trip_phases` returns a deterministic order across 32 repeated calls.

## Independent test charter

Falsify the claim that display never comes from camera metadata.

- Build your own trip resembling the real target pattern: Tokyo, a Pacific
  crossing, Ushuaia, a Drake Passage transit, Antarctica, and the return — six
  phases, two of them transit, at least one spanning a date-line crossing and one
  spanning a daylight-saving change.
- Deliberately set every asset's camera metadata timezone to something wrong and
  assert no displayed time reflects it.
- Attack coverage: a clip exactly on a phase's `start_trip_time`; a clip exactly on
  its `end_trip_time`; a zero-width phase; a phase whose end precedes its start
  (which must be rejected); three phases overlapping the same instant; a gap
  between two phases.
- Attack `chrono-tz` usage with `Antarctica/McMurdo` and `Pacific/Kiritimati`,
  which have unusual offsets, and with a timezone whose offset changed
  historically.
- Assert determinism by shuffling the phase list across 64 seeds.
- Confirm removing a phase changes no trip time by comparing the full trip-time map
  before and after.

## Review charter

Risk domain: timezone handling, which is where correct-looking code is most often
wrong, and display authority leaking back to camera metadata.

Questions:

1. Is any displayed time derived from camera metadata timezone?
2. Is any fixed offset stored or computed where an IANA zone was required?
3. Is the coverage rule total, including boundaries, gaps, overlaps, and zero-width
   phases?
4. For dual mode, is primary origin and secondary destination, per the frozen
   decision?
5. Does removing a phase alter any trip time?
6. Is the phase list ordering deterministic?

Severity rules: display derived from camera metadata, a stored fixed offset, a
non-total coverage rule, or a trip time changed by a phase edit is blocking.
Formatting of the rendered string is non-blocking provided it is valid RFC 3339
with offset.

Smallest acceptable remedy: correct the resolution path and add the timezone case
that exposed the error.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Any UI for editing phases, including the trip-context band and boundary
  dragging, which are D01-T015 and D01-T016.
- Automatic phase inference from GPS, geocoding, or clip content.
- Clock anchors, deferred by `PRD.md` §21.3.
- Changing epoch or exception behavior.
- Timezone data bundling decisions beyond using the existing `chrono-tz`
  dependency.

## Stop conditions

Stop and escalate when:

- the coverage rule cannot be made total for a real phase arrangement without a
  product decision;
- dual display needs a third timezone to express a real transit;
- `chrono-tz` cannot resolve a timezone the target trip requires;
- populating `DisplayTime` requires changing `solver.rs`, which this packet does not
  own;
- a phase edit turns out to require retiming assets, which would contradict the
  frozen decision that phases carry no membership.

## Commands

```sh
cargo test -p rcdvlog-core --test chronology_phases
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected result: all tests pass and clippy is clean.

## Handoff

Report the coverage rule as implemented, the date-line and daylight-saving results
with concrete before and after strings, the dual-mode ordering, the uncovered-clip
behavior, the determinism evidence, and confirmation that camera metadata timezone
is never a display source. List any stop condition encountered. The implementer
does not approve this task.

## Promotion note

`draft` until D01-T007 publishes `docs/tasks/desktop/CHRONOLOGY_MODEL.md` and
D01-T008 lands the solver. The contract editor confirms `DisplayTime` and
`DisplayMode` match the frozen definitions before promoting.
