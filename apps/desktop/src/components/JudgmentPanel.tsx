import { useEffect, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import type { Asset, AssetAnalysisView } from "../types";
import type { SaveState } from "../session/assetMutations";
import { suggestLabels, type LabelUse } from "../review/labelVocabulary";

export type { SaveState };

export interface JudgmentPanelProps {
  asset: Asset;
  /** 1-based queue position — the coordinate shared with the queue and ribbon. */
  index: number;
  /** Read only for the two permitted exceptions: AI labels and the ⚠ chip. */
  observation: AssetAnalysisView | null;
  labelDraft: string;
  /** Labels this project already uses, most-used first — the typeahead's source. */
  labelVocabulary: readonly LabelUse[];
  saveState: SaveState;
  /** Why the last save was refused, shown as the unsaved state's reason. */
  saveError?: string;
  /** Send the retained draft again, unchanged. */
  onRetrySave?(): void;
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
  // Not "Error": what the user needs to know is where their paragraph stands,
  // which is on screen and not in the catalog.
  unsaved: "Unsaved",
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
  labelVocabulary,
  saveState,
  saveError = "",
  onRetrySave,
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

  // The typeahead is opened by typing, never by focus: `L` → type → Enter is
  // the review loop this field exists for, and a list that appears on focus
  // would put a highlighted suggestion in front of the very first keystroke.
  const [vocabularyOpen, setVocabularyOpen] = useState(false);
  const [activeSuggestion, setActiveSuggestion] = useState(-1);
  const matches = vocabularyOpen
    ? suggestLabels(labelVocabulary, labelDraft, { exclude: asset.labels })
    : [];
  const active = activeSuggestion >= 0 && activeSuggestion < matches.length
    ? matches[activeSuggestion]
    : null;

  // Moving to another clip must not leave a list open over the new one's rail.
  useEffect(() => {
    setVocabularyOpen(false);
    setActiveSuggestion(-1);
  }, [asset.id]);

  function commitLabel(value: string) {
    setVocabularyOpen(false);
    setActiveSuggestion(-1);
    onAddLabel(value);
  }

  function onLabelKeyDown(event: ReactKeyboardEvent<HTMLInputElement>) {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      const list = vocabularyOpen
        ? matches
        : suggestLabels(labelVocabulary, labelDraft, { exclude: asset.labels });
      if (!list.length) return;
      event.preventDefault();
      setVocabularyOpen(true);
      const step = event.key === "ArrowDown" ? 1 : -1;
      // Wraps through -1, which is "nothing highlighted": the way back to
      // committing exactly what was typed has to be reachable from the keys.
      setActiveSuggestion((current) => {
        const next = current + step;
        if (next >= list.length) return -1;
        if (next < -1) return list.length - 1;
        return next;
      });
      return;
    }
    if (event.key === "Escape" && vocabularyOpen) {
      // The shell owns Escape (issue #52); this one is genuinely the field's,
      // so it is consumed here rather than closing the list *and* leaving Focus.
      event.preventDefault();
      event.stopPropagation();
      setVocabularyOpen(false);
      setActiveSuggestion(-1);
      return;
    }
    if (event.key === "Enter" || event.key === ",") {
      event.preventDefault();
      // A highlighted suggestion is one the user deliberately arrowed to.
      // Otherwise Enter still commits exactly what was typed.
      commitLabel(active?.label ?? labelDraft);
      return;
    }
    if (event.key === "Backspace" && !labelDraft && asset.labels.length > 0) {
      onRemoveLabel(asset.labels.at(-1) ?? "");
    }
  }

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

      {/* A refused save leaves the draft on screen, so the header has to say
          so: an unmarked draft reads exactly like saved text, and the user
          would close the app believing it was written. The retry sends the
          same draft, with no edit needed to retrigger a blur. */}
      <div className="micro-header">
        <span>Notes</span>
        <span className={`save-state ${saveState === "unsaved" ? "unsaved" : ""}`}>
          {saveStateLabels[saveState]}
          {saveState === "unsaved" && onRetrySave && (
            <button
              className="save-retry"
              onClick={onRetrySave}
              title={saveError ? `Not saved: ${saveError}` : "Save this note again"}
            >Retry</button>
          )}
        </span>
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
      <div className="label-combobox">
        <input
          id="label-input"
          className="label-field"
          value={labelDraft}
          placeholder="+ add label"
          aria-label="Add label"
          aria-keyshortcuts="L"
          role="combobox"
          aria-expanded={matches.length > 0}
          aria-controls="label-vocabulary"
          aria-autocomplete="list"
          aria-activedescendant={active ? `label-vocabulary-${activeSuggestion}` : undefined}
          onChange={(event) => {
            onLabelDraftChange(event.currentTarget.value);
            setVocabularyOpen(true);
            setActiveSuggestion(-1);
          }}
          onKeyDown={onLabelKeyDown}
          onBlur={() => setVocabularyOpen(false)}
        />
        {matches.length > 0 && (
          <ul className="label-vocabulary" id="label-vocabulary" role="listbox" aria-label="Labels already in this project">
            {matches.map((use, position) => (
              <li
                key={use.label}
                id={`label-vocabulary-${position}`}
                role="option"
                aria-selected={position === activeSuggestion}
                className={position === activeSuggestion ? "active" : ""}
                // Commit before the input's blur can close the list underneath
                // the click.
                onMouseDown={(event) => { event.preventDefault(); commitLabel(use.label); }}
              >
                <span>{use.label}</span>
                <em>{use.count}</em>
              </li>
            ))}
          </ul>
        )}
      </div>
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
