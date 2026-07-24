import type { ReactNode } from "react";

export interface StatusBadgeProps {
  label: string;
  tone?: "neutral" | "positive" | "warning" | "critical";
  className?: string;
}

export function StatusBadge({ label, tone = "neutral", className = "" }: StatusBadgeProps) {
  return (
    <span className={`review-status-badge review-status-${tone} ${className}`.trim()}>
      <span className="review-status-dot" aria-hidden="true" />
      {label}
    </span>
  );
}

export interface StatePanelProps {
  title: string;
  children: ReactNode;
  action?: ReactNode;
  busy?: boolean;
}

export function StatePanel({ title, children, action, busy = false }: StatePanelProps) {
  return (
    <section className="review-state-panel" aria-live={busy ? "polite" : undefined} aria-busy={busy || undefined}>
      <h2>{title}</h2>
      <div>{children}</div>
      {action && <div className="review-state-action">{action}</div>}
    </section>
  );
}
