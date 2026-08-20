/**
 * Proposing a display timezone from project evidence (#72), with nothing
 * rendered. The invariants that matter here are all about restraint: the
 * suggestion comes from the project's own phases, names where it came from,
 * and never appears when there is nothing to base it on.
 */

import { describe, expect, it } from "vitest";
import { describeTimezoneSuggestion, suggestTimezoneFromPhases } from "./displayTimezone";
import type { TripPhase } from "../chronologyTypes";

function phase(overrides: Partial<TripPhase> = {}): TripPhase {
  return {
    id: "phase_1",
    label: "Barcelona",
    kind: "location",
    startAnchorAssetId: null,
    endAnchorAssetId: null,
    startTripTime: "2026-08-01T00:00:00Z",
    endTripTime: "2026-08-05T00:00:00Z",
    timezone: "Europe/Madrid",
    originTimezone: null,
    destinationTimezone: null,
    displayMode: "local",
    ...overrides,
  };
}

describe("suggestTimezoneFromPhases", () => {
  it("proposes the nearest phase's zone, and says which phase it came from", () => {
    const found = suggestTimezoneFromPhases([phase()], "2026-08-06T09:00:00Z");
    expect(found).toEqual({ timezone: "Europe/Madrid", phaseLabel: "Barcelona", direction: "before" });
    expect(describeTimezoneSuggestion(found!)).toBe(
      "Suggested from “Barcelona”, the trip context just before this clip.",
    );
  });

  it("prefers the phase closest in time, not the first one declared", () => {
    const far = phase({ id: "phase_far", label: "Tokyo", timezone: "Asia/Tokyo", startTripTime: "2026-07-01T00:00:00Z", endTripTime: "2026-07-05T00:00:00Z" });
    const near = phase({ id: "phase_near", label: "Barcelona", timezone: "Europe/Madrid" });
    expect(suggestTimezoneFromPhases([far, near], "2026-08-05T06:00:00Z")?.timezone).toBe("Europe/Madrid");
    expect(suggestTimezoneFromPhases([near, far], "2026-07-06T00:00:00Z")?.timezone).toBe("Asia/Tokyo");
  });

  it("proposes a later phase's zone when the clip precedes every phase", () => {
    const found = suggestTimezoneFromPhases([phase()], "2026-07-30T12:00:00Z");
    expect(found).toEqual({ timezone: "Europe/Madrid", phaseLabel: "Barcelona", direction: "after" });
    expect(describeTimezoneSuggestion(found!)).toContain("just after this clip");
  });

  it("takes the near side of a transit phase: origin before departure, destination after arrival", () => {
    const flight = phase({
      id: "phase_flight",
      label: "Flight to Toronto",
      kind: "transit",
      timezone: null,
      originTimezone: "Asia/Tokyo",
      destinationTimezone: "America/Toronto",
    });
    expect(suggestTimezoneFromPhases([flight], "2026-07-31T12:00:00Z")?.timezone).toBe("Asia/Tokyo");
    expect(suggestTimezoneFromPhases([flight], "2026-08-06T12:00:00Z")?.timezone).toBe("America/Toronto");
  });

  it("proposes nothing when a phase already covers the clip: that answer is authoritative", () => {
    expect(suggestTimezoneFromPhases([phase()], "2026-08-03T00:00:00Z")).toBeNull();
  });

  it("proposes nothing without project evidence, and never invents one", () => {
    expect(suggestTimezoneFromPhases([], "2026-08-06T00:00:00Z")).toBeNull();
    // A phase that names a place but no zone has nothing to lend.
    expect(suggestTimezoneFromPhases([phase({ timezone: null })], "2026-08-06T00:00:00Z")).toBeNull();
    expect(suggestTimezoneFromPhases([phase({ timezone: "   " })], "2026-08-06T00:00:00Z")).toBeNull();
  });

  it("proposes nothing for a clip with no placeable time, or an unreadable one", () => {
    expect(suggestTimezoneFromPhases([phase()], null)).toBeNull();
    expect(suggestTimezoneFromPhases([phase()], "not a timestamp")).toBeNull();
  });

  it("ignores a phase whose own bounds cannot be read rather than guessing past it", () => {
    const broken = phase({ id: "phase_broken", label: "Broken", timezone: "Asia/Tokyo", startTripTime: "??", endTripTime: "??" });
    expect(suggestTimezoneFromPhases([broken, phase()], "2026-08-06T00:00:00Z")?.timezone).toBe("Europe/Madrid");
  });
});
