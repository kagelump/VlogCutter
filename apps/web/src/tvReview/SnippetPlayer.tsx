import { useState } from "react";

/**
 * One bounded listening snippet. Nothing is fetched until the reviewer asks:
 * the audio element is mounted only after the click, so opening a page never
 * downloads private audio by itself.
 */
export function SnippetPlayer({ url, label }: { url: string; label: string }) {
  const [open, setOpen] = useState(false);
  const [failed, setFailed] = useState(false);
  return <span className="tv-snippet">
    {open
      ? <audio controls autoPlay preload="none" src={url} aria-label={`Recording ${label}`} onError={() => setFailed(true)} />
      : <button type="button" className="link-button" onClick={() => setOpen(true)} aria-label={`Listen to ${label}`}>▶ {label}</button>}
    {failed ? <small role="alert">This span could not be played: it is missing, deleted, or no longer matches its stored hash.</small> : null}
  </span>;
}
