import type { CardCandidate } from "../types";

export function CardBanner({ card, onReview, onDismiss }: { card: CardCandidate; onReview(): void; onDismiss(): void }) {
  return (
    <aside className="card-banner" aria-label="Camera card detected">
      <span className="card-banner-pulse" aria-hidden="true" />
      <strong>{card.label} detected</strong>
      <span>{card.cameraHint || "Camera card"} · {card.clipCount} {card.clipCount === 1 ? "clip" : "clips"}</span>
      <button className="button primary" onClick={onReview}>Review import</button>
      <button className="banner-dismiss" aria-label={`Dismiss ${card.label}`} onClick={onDismiss}>×</button>
    </aside>
  );
}
