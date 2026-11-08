import { useQuery } from "@tanstack/react-query";
import { keys, useClient } from "../../app/client";
import { formatDateTime } from "../../lib/format";
import { ErrorMessage, Loading } from "../../ui";
import { ADMIN_POLL_MS, Section } from "./shared";
import styles from "./Admin.module.css";

export function ActivitySection() {
  const client = useClient();
  const audit = useQuery({ queryKey: [...keys.admin, "audit"], queryFn: () => client.listAdminAudit(30), refetchInterval: ADMIN_POLL_MS });
  return (
    <Section id="activity" title="Activity" description="The latest admin changes.">
      {audit.isPending ? <Loading /> : null}
      <ErrorMessage error={audit.error} />
      {audit.data ? (
        audit.data.items.length ? (
          <ol className={`${styles.rows} ${styles.audit}`}>
            {audit.data.items.map((event) => (
              <li key={event.eventSequence} title={`Request ${event.requestId}`}>
                <code>{event.action}</code>
                <time dateTime={event.occurredAt}>{formatDateTime(event.occurredAt)}</time>
                <span>{event.actorType} {event.actorId}{event.targetId ? ` → ${event.targetId}` : ""}</span>
              </li>
            ))}
          </ol>
        ) : <p className={styles.none}>Nothing yet.</p>
      ) : null}
    </Section>
  );
}
