import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import type { Asset, AssetAnalysisView } from "../types";

export type SaveState = "idle" | "saving" | "saved";

export interface JudgmentPanelProps {
  asset: Asset;
  /** 1-based queue position — the coordinate shared with the queue and ribbon. */
  index: number;
  /** Read only for the two permitted exceptions: AI labels and the ⚠ chip. */
  observation: AssetAnalysisView | null;
  labelDraft: string;
  saveState: SaveState;
  onLabelDraftChange(value: string): void;
  onAddLabel(value: string): void;
  onRemoveLabel(label: string): void;
  onPromoteAiLabel(label: string): void;
  onChangeAsset(next: Asset): void;
  onCommitAsset(): void;
  onToggleRemoved(): void;
  onNotesKeyDown(event: ReactKeyboardEvent<HTMLTextAreaElement>): void;
  onNotesFocus(): void;
  onNotesBlur(): void;
}

const saveStateLabels: Record<SaveState, string> = {
  idle: "",
  saving: "Saving…",
  saved: "Saved",
};

/**
 * The right rail (spec §3.7).
 *
 * RULE 1 — this panel holds human-authored content only. Notes, labels,
 * title, and removal. No read-only field, no machine output, no metadata may
 * be added here. Exactly two exceptions exist and both are narrow:
 *
 *   - the AI suggestion strip, because its interaction is authoring (promote
 *     a suggestion into a real label) rather than reading;
 *   - the ⚠ unusable chip, because it prompts the human decision this rail
 *     exists to capture. Its reason text lives in the shelf's Analysis tab.
 *
 * Anything else machine-authored belongs in the centre column. This is a rule
 * rather than an ordering because orderings decay: the previous inspector
 * accumulated nine sections and pushed the notes field 81px below the fold.
 *
 * RULE 3 — the notes field has a fixed height and a fixed position. The grid
 * rows below are fixed and the panel does not stretch, so opening or closing
 * anything else cannot move it.
 */
export function JudgmentPanel({
  asset,
  index,
  observation,
  labelDraft,
  saveState,
  onLabelDraftChange,
  onAddLabel,
  onRemoveLabel,
  onPromoteAiLabel,
  onChangeAsset,
  onCommitAsset,
  onToggleRemoved,
  onNotesKeyDown,
  onNotesFocus,
  onNotesBlur,
}: JudgmentPanelProps) {
  const unusable = observation && !observation.analysis.usable;
  // A suggestion you already accepted is not a suggestion. Without this every
  // promoted label stays in the strip, duplicating the chip directly above it.
  const taken = new Set(asset.labels.map((label) => label.toLocaleLowerCase()));
  const suggestions = (observation?.aiLabels ?? []).filter(
    (label) => !taken.has(label.toLocaleLowerCase()),
  );

  return (
    <aside className="judgment-panel">
      <div className="judgment-header">
        <span className="clip-index">#{index > 0 ? String(index).padStart(3, "0") : "—"}</span>
        <input
          className="clip-title-field"
          value={asset.title}
          placeholder={asset.filename}
          aria-label="Clip title"
          onChange={(event) => onChangeAsset({ ...asset, title: event.target.value })}
          onBlur={onCommitAsset}
          onKeyDown={(event) => {
            if (event.key === "Enter") event.currentTarget.blur();
          }}
        />
        {unusable && (
          <span
            className="unusable-chip"
            title={`Analysis flagged this clip: ${observation.analysis.unusableReason ?? "unusable"}. Your call — this does not remove it.`}
          >⚠</span>
        )}
        <button
          className="header-action"
          onClick={onToggleRemoved}
          aria-keyshortcuts="R"
          aria-label={asset.removed ? "Restore clip" : "Remove from labeling"}
          title={asset.removed ? "Restore clip (R)" : "Remove from labeling (R)"}
        >{asset.removed ? "Restore" : "Remove"}</button>
      </div>

      <div className="micro-header">
        <span>Notes</span>
        <span className="save-state">{saveStateLabels[saveState]}</span>
      </div>
      {/* The largest element on screen, and the only thing in the app at
          15px: the hierarchy is stated in the type scale rather than in
          chrome. 18 visible lines says "write the thing you will want to
          read in eight months". */}
      <textarea
        id="notes"
        className="notes-field"
        value={asset.notes}
        placeholder="What happens here? What might this cut with?"
        aria-label="Notes"
        aria-keyshortcuts="Meta+Enter Shift+Enter Escape"
        onFocus={onNotesFocus}
        onChange={(event) => onChangeAsset({ ...asset, notes: event.target.value })}
        onKeyDown={onNotesKeyDown}
        onBlur={onNotesBlur}
      />

      <div className="micro-header"><span>Labels</span></div>
      <input
        id="label-input"
        className="label-field"
        value={labelDraft}
        placeholder="+ add label"
        aria-label="Add label"
        aria-keyshortcuts="L"
        onChange={(event) => onLabelDraftChange(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === ",") {
            event.preventDefault();
            onAddLabel(labelDraft);
          } else if (event.key === "Backspace" && !labelDraft && asset.labels.length > 0) {
            onRemoveLabel(asset.labels.at(-1) ?? "");
          }
        }}
      />
      <div className="chip-well" aria-label="Clip labels">
        {asset.labels.map((label) => (
          <button key={label} onClick={() => onRemoveLabel(label)} aria-label={`Remove ${label}`}>
            <span>{label}</span><i aria-hidden="true">×</i>
          </button>
        ))}
      </div>

      <div className="ai-strip">
        {suggestions.length > 0 && <span className="ai-prefix">ai</span>}
        {suggestions.map((label) => (
          <button
            className="ai-suggestion"
            key={label}
            onClick={() => onPromoteAiLabel(label)}
            title={`Add ${label} as your label`}
          >{label}</button>
        ))}
      </div>
    </aside>
  );
}
