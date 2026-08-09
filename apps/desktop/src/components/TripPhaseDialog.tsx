import { useEffect, useState } from "react";
import type { Asset } from "../types";
import type { DisplayMode, PhaseKind, TripPhase } from "../chronologyTypes";

export interface TripPhaseFormValue {
  label: string;
  kind: PhaseKind;
  timezone: string | null;
  originTimezone: string | null;
  destinationTimezone: string | null;
  displayMode: DisplayMode;
}

interface Props {
  phase: TripPhase | null;
  start: Asset;
  end: Asset;
  startPosition: number;
  endPosition: number;
  busy: boolean;
  error: string;
  onCancel(): void;
  onSave(value: TripPhaseFormValue): void;
  onRemove?(): void;
}

function clean(value: string) {
  const trimmed = value.trim();
  return trimmed === "" ? null : trimmed;
}

export function TripPhaseDialog({
  phase,
  start,
  end,
  startPosition,
  endPosition,
  busy,
  error,
  onCancel,
  onSave,
  onRemove,
}: Props) {
  const [label, setLabel] = useState(phase?.label ?? "");
  const [kind, setKind] = useState<PhaseKind>(phase?.kind ?? "location");
  const [timezone, setTimezone] = useState(phase?.timezone ?? "");
  const [originTimezone, setOriginTimezone] = useState(phase?.originTimezone ?? "");
  const [destinationTimezone, setDestinationTimezone] = useState(phase?.destinationTimezone ?? "");
  const [displayMode, setDisplayMode] = useState<DisplayMode>(phase?.displayMode ?? "local");
  const clipCount = endPosition - startPosition + 1;

  useEffect(() => {
    function closeOnEscape(event: KeyboardEvent) {
      if (event.key === "Escape" && !busy) onCancel();
    }
    document.addEventListener("keydown", closeOnEscape);
    return () => document.removeEventListener("keydown", closeOnEscape);
  }, [busy, onCancel]);

  return (
    <div
      className="modal-backdrop context-modal-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && !busy) onCancel();
      }}
    >
      <section
        className="modal context-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="trip-phase-title"
      >
        <header className="context-modal-header">
          <div>
            <span className="eyebrow">Trip context</span>
            <h2 id="trip-phase-title">{phase ? `Edit ${phase.label}` : "Add context to the ribbon"}</h2>
          </div>
          <button type="button" className="context-modal-close" aria-label="Close context dialog" disabled={busy} onClick={onCancel}>×</button>
        </header>

        <form
          className="context-phase-form"
          onSubmit={(event) => {
            event.preventDefault();
            onSave({
              label: label.trim(),
              kind,
              timezone: kind === "location" ? clean(timezone) : null,
              originTimezone: kind === "transit" ? clean(originTimezone) : null,
              destinationTimezone: kind === "transit" ? clean(destinationTimezone) : null,
              displayMode,
            });
          }}
        >
          <div className="context-modal-body">
            <div className="context-range-preview" aria-label="Selected clip range">
              <div className="context-range-heading">
                <span>Anchored clip range</span>
                <em>{clipCount} clip{clipCount === 1 ? "" : "s"}</em>
              </div>
              <div>
                <span>From</span>
                <strong>#{startPosition + 1} · {start.filename}</strong>
              </div>
              <i aria-hidden="true" />
              <div>
                <span>Through</span>
                <strong>#{endPosition + 1} · {end.filename}</strong>
              </div>
            </div>

            <fieldset className="context-kind-picker">
              <legend>What kind of context is this?</legend>
              <label className={kind === "location" ? "active" : ""}>
                <input type="radio" name="phase-kind" value="location" checked={kind === "location"} onChange={() => setKind("location")} />
                <strong>Location</strong>
                <span>A place where you stayed or explored</span>
              </label>
              <label className={kind === "transit" ? "active" : ""}>
                <input type="radio" name="phase-kind" value="transit" checked={kind === "transit"} onChange={() => { setKind("transit"); setDisplayMode("dual"); }} />
                <strong>Transit</strong>
                <span>A flight, train, drive, or crossing</span>
              </label>
            </fieldset>

            <label className="context-form-field">
              Name
              <input
                autoFocus
                value={label}
                onChange={(event) => setLabel(event.target.value)}
                placeholder={kind === "location" ? "Osaka" : "Flight to Toronto"}
                required
              />
            </label>

            {kind === "location" ? (
              <label className="context-form-field">
                Timezone
                <input value={timezone} onChange={(event) => setTimezone(event.target.value)} placeholder="Asia/Tokyo" />
                <small>Use an IANA timezone. Leave blank if you only want a named place.</small>
              </label>
            ) : (
              <div className="context-transit-fields">
                <label className="context-form-field">
                  Origin timezone
                  <input value={originTimezone} onChange={(event) => setOriginTimezone(event.target.value)} placeholder="Asia/Tokyo" />
                </label>
                <label className="context-form-field">
                  Destination timezone
                  <input value={destinationTimezone} onChange={(event) => setDestinationTimezone(event.target.value)} placeholder="America/Toronto" />
                </label>
                <label className="context-form-field">
                  Display time
                  <select value={displayMode} onChange={(event) => setDisplayMode(event.target.value as DisplayMode)}>
                    <option value="dual">Both timezones</option>
                    <option value="local">Origin timezone</option>
                    <option value="utc">UTC</option>
                  </select>
                </label>
              </div>
            )}

            {error && <p className="context-modal-error" role="alert">{error}</p>}
          </div>

          <div className="modal-actions context-modal-actions">
            <div>
              {phase && onRemove && (
                <button type="button" className="button context-remove-button" disabled={busy} onClick={onRemove}>Remove phase</button>
              )}
            </div>
            <div className="context-modal-confirm">
              <button type="button" className="button" disabled={busy} onClick={onCancel}>Cancel</button>
              <button type="submit" className="button primary" disabled={busy || !label.trim()}>
                {busy ? "Saving…" : phase ? "Save phase" : "Add phase"}
              </button>
            </div>
          </div>
        </form>
      </section>
    </div>
  );
}
