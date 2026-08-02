//! Trip-phase coverage and local display time (D01-T012).
//!
//! Local display time derives from the covering trip phase, never from camera
//! metadata timezone (architecture contract §3.4). Timezones are IANA
//! identifiers resolved through `chrono-tz`, so a daylight-saving boundary
//! inside a phase resolves correctly instead of being frozen into a stored
//! offset.
//!
//! Everything here is pure: no I/O, no clock reads, no unordered collections.

use super::model::{DisplayMode, PhaseKind, TripPhase};
use chrono::{DateTime, SecondsFormat, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// A trip time rendered for a human, with the timezone it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayTime {
    pub primary: String,
    pub secondary: Option<String>,
    pub timezone: String,
    pub phase_id: Option<String>,
}

/// Parses an RFC 3339 timestamp into UTC. Returns `None` rather than erroring,
/// because a malformed persisted string is evidence of a bad write, not a
/// reason for the solver to fail the whole project.
pub fn parse_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

/// Resolves an IANA identifier. An unknown or absent identifier falls back to
/// UTC, which is recorded as evidence by the caller rather than failing.
pub fn resolve_timezone(name: Option<&str>) -> (Tz, bool) {
    match name {
        Some(raw) => match raw.parse::<Tz>() {
            Ok(zone) => (zone, true),
            Err(_) => (Tz::UTC, false),
        },
        None => (Tz::UTC, false),
    }
}

/// True when `name` is a resolvable IANA identifier. Used by write paths to
/// reject an invalid timezone before anything is persisted.
pub fn is_valid_timezone(name: &str) -> bool {
    name.parse::<Tz>().is_ok()
}

/// The phase covering this trip time, by the frozen coverage rule.
///
/// Intervals are inclusive at both ends. Overlap is expected and is resolved
/// deterministically: the phase with the later `start_trip_time` wins, and on
/// an exact tie the greater `id` wins. `TripPhase` carries no HLC field, so the
/// id — unique and stably ordered — is the tie-break of record. When no phase
/// contains the trip time the result is `None` and the caller displays UTC.
pub fn covering_phase<'a>(phases: &'a [TripPhase], trip_time: &str) -> Option<&'a TripPhase> {
    let instant = parse_utc(trip_time)?;
    let mut winner: Option<&TripPhase> = None;
    for phase in phases {
        let (Some(start), Some(end)) = (
            parse_utc(&phase.start_trip_time),
            parse_utc(&phase.end_trip_time),
        ) else {
            continue;
        };
        if start > end || instant < start || instant > end {
            continue;
        }
        let better = match winner {
            None => true,
            Some(current) => {
                let current_start = parse_utc(&current.start_trip_time);
                match (current_start, Some(start)) {
                    (Some(existing), Some(candidate)) if candidate != existing => {
                        candidate > existing
                    }
                    _ => phase.id > current.id,
                }
            }
        };
        if better {
            winner = Some(phase);
        }
    }
    winner
}

/// Renders one instant in one zone as an RFC 3339 string with a real offset.
fn render(instant: DateTime<Utc>, zone: Tz) -> String {
    instant
        .with_timezone(&zone)
        .to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// The display time for a trip time under whichever phase covers it.
///
/// A `None` trip time yields `None`. An uncovered trip time is displayed
/// honestly as UTC with a null `phase_id` rather than guessed at. For a transit
/// phase `primary` uses `origin_timezone` and `secondary` uses
/// `destination_timezone`, so the user reads departure-local first.
pub fn display_time(phases: &[TripPhase], trip_time: Option<&str>) -> Option<DisplayTime> {
    let raw = trip_time?;
    let instant = parse_utc(raw)?;
    let Some(phase) = covering_phase(phases, raw) else {
        return Some(DisplayTime {
            primary: render(instant, Tz::UTC),
            secondary: None,
            timezone: "UTC".to_string(),
            phase_id: None,
        });
    };

    let primary_name = match phase.kind {
        PhaseKind::Transit => phase
            .origin_timezone
            .as_deref()
            .or(phase.timezone.as_deref()),
        PhaseKind::Location => phase.timezone.as_deref(),
    };
    let (primary_zone, primary_named) = resolve_timezone(primary_name);

    let (zone, label) = match phase.display_mode {
        DisplayMode::Utc => (Tz::UTC, "UTC".to_string()),
        _ if primary_named => (primary_zone, primary_name.unwrap_or("UTC").to_string()),
        _ => (Tz::UTC, "UTC".to_string()),
    };

    let secondary = match phase.display_mode {
        DisplayMode::Dual => {
            let secondary_name = match phase.kind {
                PhaseKind::Transit => phase.destination_timezone.as_deref(),
                PhaseKind::Location => None,
            };
            let (secondary_zone, _) = resolve_timezone(secondary_name);
            Some(render(instant, secondary_zone))
        }
        _ => None,
    };

    Some(DisplayTime {
        primary: render(instant, zone),
        secondary,
        timezone: label,
        phase_id: Some(phase.id.clone()),
    })
}

/// Whether the phase covering this trip time named a timezone that resolved.
/// The solver turns this into evidence so a UTC fallback is visible to the
/// user rather than silent.
pub(crate) fn coverage_evidence(phases: &[TripPhase], trip_time: &str) -> Option<(String, bool)> {
    let phase = covering_phase(phases, trip_time)?;
    let name = match phase.kind {
        PhaseKind::Transit => phase
            .origin_timezone
            .as_deref()
            .or(phase.timezone.as_deref()),
        PhaseKind::Location => phase.timezone.as_deref(),
    };
    let (_, named) = resolve_timezone(name);
    Some((phase.id.clone(), named))
}
