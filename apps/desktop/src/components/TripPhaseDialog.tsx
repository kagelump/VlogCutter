import { useEffect, useRef, useState } from "react";
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
  assets?: Asset[];
  busy: boolean;
  error: string;
  onCancel(): void;
  onSave(value: TripPhaseFormValue, range?: { startPosition: number; endPosition: number }): void;
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
  assets,
  busy,
  error,
  onCancel,
  onSave,
  onRemove,
}: Props) {
  // Capture the opener during render, before the Name field's `autoFocus`
  // runs. A passive effect would see the dialog field and restore focus to a
  // detached node after Save/Cancel.
  const openerRef = useRef<HTMLElement | null>(
    typeof document !== "undefined" && document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null,
  );
  const [label, setLabel] = useState(phase?.label ?? "");
  const [kind, setKind] = useState<PhaseKind>(phase?.kind ?? "location");
  const [timezone, setTimezone] = useState(phase?.timezone ?? "");
  const [originTimezone, setOriginTimezone] = useState(phase?.originTimezone ?? "");
  const [destinationTimezone, setDestinationTimezone] = useState(phase?.destinationTimezone ?? "");
  const [displayMode, setDisplayMode] = useState<DisplayMode>(phase?.displayMode ?? "local");
  const [rangeStart, setRangeStart] = useState(startPosition);
  const [rangeEnd, setRangeEnd] = useState(endPosition);
  const dialogRef = useRef<HTMLElement>(null);
  const cancelRef = useRef(onCancel);
  const busyRef = useRef(busy);
  cancelRef.current = onCancel;
  busyRef.current = busy;
  const rangeStartAsset = assets?.[rangeStart] ?? start;
  const rangeEndAsset = assets?.[rangeEnd] ?? end;
  const clipCount = rangeEnd - rangeStart + 1;

  useEffect(() => {
    const focusable = () => [...(dialogRef.current?.querySelectorAll<HTMLElement>(
      'button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])',
    ) ?? [])].filter((element) => element.offsetParent !== null || element === document.activeElement);
    function containFocus(event: KeyboardEvent) {
      if (event.key === "Escape" && !busyRef.current) {
        event.preventDefault();
        event.stopPropagation();
        cancelRef.current();
        return;
      }
      if (event.key !== "Tab") return;
      const items = focusable();
      if (!items.length) return;
      const first = items[0];
      const last = items.at(-1)!;
      const focusOutside = !dialogRef.current?.contains(document.activeElement);
      if (event.shiftKey && (document.activeElement === first || focusOutside)) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (document.activeElement === last || focusOutside)) {
        event.preventDefault();
        first.focus();
      }
    }
    document.addEventListener("keydown", containFocus);
    return () => {
      document.removeEventListener("keydown", containFocus);
      openerRef.current?.focus();
    };
  }, []);

  return (
    <div
      className="modal-backdrop context-modal-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && !busy) onCancel();
      }}
    >
      <section
        ref={dialogRef}
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
            }, { startPosition: rangeStart, endPosition: rangeEnd });
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
                <strong>#{rangeStart + 1} · {rangeStartAsset.filename}</strong>
              </div>
              <i aria-hidden="true" />
              <div>
                <span>Through</span>
                <strong>#{rangeEnd + 1} · {rangeEndAsset.filename}</strong>
              </div>
            </div>

            {phase && assets && (
              <fieldset className="context-boundary-editor">
                <legend>Adjust anchored boundaries</legend>
                <label>
                  Start clip
                  <input
                    type="range"
                    min={0}
                    max={rangeEnd}
                    value={rangeStart}
                    onChange={(event) => setRangeStart(Math.min(Number(event.target.value), rangeEnd))}
                    aria-valuetext={`Clip ${rangeStart + 1}, ${rangeStartAsset.filename}`}
                  />
                  <span>#{rangeStart + 1} · {rangeStartAsset.filename}</span>
                </label>
                <label>
                  End clip
                  <input
                    type="range"
                    min={rangeStart}
                    max={assets.length - 1}
                    value={rangeEnd}
                    onChange={(event) => setRangeEnd(Math.max(Number(event.target.value), rangeStart))}
                    aria-valuetext={`Clip ${rangeEnd + 1}, ${rangeEndAsset.filename}`}
                  />
                  <span>#{rangeEnd + 1} · {rangeEndAsset.filename}</span>
                </label>
                <small>Boundaries snap to clip identities and keep following those clips after clock corrections.</small>
              </fieldset>
            )}

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
