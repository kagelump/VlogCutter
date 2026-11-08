import type { ReactNode } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { keys } from "../../app/client";
import { Badge, type Tone } from "../../ui";
import styles from "./Admin.module.css";

export const ADMIN_POLL_MS = 15_000;

/**
 * Any admin mutation refreshes admin data once it lands. The refresh runs in
 * the background so dialogs close as soon as the server confirms.
 */
export function useAdminAction<TArgs = void, TResult = unknown>(fn: (args: TArgs) => Promise<TResult>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSettled: () => { void queryClient.invalidateQueries({ queryKey: keys.admin }); },
  });
}

export function Section({ id, title, description, actions, children }: { id: string; title: string; description?: ReactNode; actions?: ReactNode; children: ReactNode }) {
  return (
    <section id={id} className={styles.section} aria-labelledby={`${id}-title`}>
      <header className={styles.sectionHeader}>
        <div>
          <h2 id={`${id}-title`}>{title}</h2>
          {description ? <p>{description}</p> : null}
        </div>
        {actions ? <div className={styles.sectionActions}>{actions}</div> : null}
      </header>
      {children}
    </section>
  );
}

export function Row({ title, meta, status, actions, children }: { title: ReactNode; meta?: ReactNode; status?: ReactNode; actions?: ReactNode; children?: ReactNode }) {
  return (
    <li className={styles.row}>
      <div className={styles.rowMain}>
        <strong>{title}</strong>
        {meta ? <span className={styles.rowMeta}>{meta}</span> : null}
        {children}
      </div>
      {status ? <div className={styles.rowStatus}>{status}</div> : null}
      {actions ? <div className={styles.rowActions}>{actions}</div> : null}
    </li>
  );
}

export function Rows({ children, empty }: { children: ReactNode[]; empty: string }) {
  if (!children.length) return <p className={styles.none}>{empty}</p>;
  return <ul className={styles.rows}>{children}</ul>;
}

export function StateBadge({ state }: { state: string }) {
  const tone: Tone = ["failed", "rejected", "mismatch", "unavailable"].includes(state)
    ? "critical"
    : ["succeeded", "confirmed", "completed", "enabled"].includes(state)
      ? "positive"
      : ["canceled", "cancelled"].includes(state) ? "neutral" : "warning";
  return <Badge tone={tone}>{state.replaceAll("_", " ")}</Badge>;
}

export function idempotencyKey() {
  return `web-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}
