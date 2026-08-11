/**
 * The keyboard map (spec §3.9). Purely presentational: it lists the shortcuts
 * the shell resolves, and closes. The bindings live in `review/keymap.ts` and
 * in `App`'s global handler; this is their printed legend, kept in step by
 * hand — a mismatch here is a documentation bug, not a behaviour one.
 */
export function KeyboardShortcutsModal({ onClose }: { onClose(): void }) {
  return (
    <div className="modal-backdrop shortcut-backdrop" role="presentation">
      <section className="modal shortcut-modal" role="dialog" aria-modal="true" aria-labelledby="shortcut-title">
        <span className="eyebrow">Keyboard map</span>
        <div className="shortcut-modal-heading">
          <h2 id="shortcut-title">Review without leaving the keys</h2>
          <button className="text-button" onClick={onClose}>Close <kbd>Esc</kbd></button>
        </div>
        <div className="shortcut-map">
          <div><kbd>J</kbd><kbd>K</kbd><span>Next / previous clip</span></div>
          <div><kbd>Space</kbd><span>Play / pause</span></div>
          <div><kbd>L</kbd><span>Focus labels</span></div>
          <div><kbd>R</kbd><span>Remove / restore clip</span></div>
          <div><kbd>N</kbd><span>Focus notes</span></div>
          <div><kbd>⌘ Enter</kbd><span>Save note</span></div>
          <div><kbd>⇧ Enter</kbd><span>Save note and advance</span></div>
          <div><kbd>Enter</kbd><span>New line in a note</span></div>
          <div><kbd>Esc</kbd><span>Cancel note edit · leave Focus</span></div>
          <div><kbd>I</kbd><span>Edit clip title</span></div>
          <div><kbd>O</kbd><span>Rotate 90° clockwise</span></div>
          <div><kbd>⇧O</kbd><span>Flip 180°</span></div>
          <div><kbd>F</kbd><span>Focus writing view</span></div>
          <div><kbd>H</kbd><span>Hide every overlay in Focus</span></div>
          <div><kbd>D</kbd><span>Evidence shelf · Focus right stack</span></div>
          <div><kbd>T</kbd><span>Collapse story ribbon</span></div>
          <div><kbd>B</kbd><span>Jump to the best moment</span></div>
          <div><kbd>⌘1</kbd><kbd>⌘2</kbd><kbd>⌘3</kbd><span>Review / Chronology / Edit plan</span></div>
          <div><kbd>/</kbd><span>Search clips</span></div>
          <div><kbd>[</kbd><kbd>]</kbd><span>Zoom story ribbon</span></div>
          <div><kbd>C</kbd><span>Center selected clip in ribbon</span></div>
          <div><kbd>P</kbd><span>Proxy selected clip</span></div>
          <div><kbd>⇧P</kbd><span>Generate missing proxies</span></div>
          <div><kbd>⇧G</kbd><span>Generate missing thumbnails</span></div>
          <div><kbd>A</kbd><span>Analyze project</span></div>
          <div><kbd>M</kbd><span>Add media</span></div>
          <div><kbd>⌘N</kbd><kbd>⌘O</kbd><span>New / open project</span></div>
          <div><kbd>⌘,</kbd><span>Open global settings</span></div>
          <div><kbd>Tab</kbd><kbd>Enter</kbd><span>Navigate / activate every control</span></div>
          <div><kbd>?</kbd><span>Open this keyboard map</span></div>
        </div>
      </section>
    </div>
  );
}
