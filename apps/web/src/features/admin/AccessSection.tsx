import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { keys, useClient } from "../../app/client";
import { formatRelative } from "../../lib/format";
import { Badge, Button, ConfirmDialog, ErrorMessage, Loading } from "../../ui";
import { ADMIN_POLL_MS, Row, Rows, Section, useAdminAction } from "./shared";
import styles from "./Admin.module.css";

type Revoke = { kind: "session" | "device"; id: string; name: string };

export function AccessSection() {
  const client = useClient();
  const session = useQuery({ queryKey: [...keys.admin, "sessions"], queryFn: () => client.getSession(), refetchInterval: ADMIN_POLL_MS });
  const devices = useQuery({ queryKey: [...keys.admin, "devices"], queryFn: () => client.listDevices(), refetchInterval: ADMIN_POLL_MS });
  const [pending, setPending] = useState<Revoke | null>(null);
  const revoke = useAdminAction((target: Revoke) => target.kind === "session" ? client.revokeSession(target.id) : client.revokeDevice(target.id));

  return (
    <Section id="access" title="Access" description="Browsers and Macs that can reach this server.">
      <ErrorMessage error={session.error ?? devices.error ?? revoke.error} />
      <div className={styles.columns}>
        <div>
          <h3>Browsers</h3>
          {session.isPending ? <Loading /> : (
            <Rows empty="No browser sessions.">
              {(session.data?.sessions ?? []).map((item) => (
                <Row
                  key={item.sessionId}
                  title={item.hostname || "Browser"}
                  meta={`${item.displayPrefix} · used ${formatRelative(item.lastUsedAt)}`}
                  status={item.revoked ? <Badge>Revoked</Badge> : null}
                  actions={!item.revoked ? <Button size="sm" variant="danger" onClick={() => setPending({ kind: "session", id: item.sessionId, name: `${item.hostname} (${item.displayPrefix})` })}>Revoke</Button> : null}
                />
              ))}
            </Rows>
          )}
        </div>
        <div>
          <h3>Paired Macs</h3>
          {devices.isPending ? <Loading /> : (
            <Rows empty="No paired Macs.">
              {(devices.data ?? []).map((item) => (
                <Row
                  key={item.credentialId}
                  title={item.label}
                  meta={`${item.tokenPrefix} · used ${formatRelative(item.lastUsedAt)}`}
                  status={item.revoked ? <Badge>Revoked</Badge> : null}
                  actions={!item.revoked ? <Button size="sm" variant="danger" onClick={() => setPending({ kind: "device", id: item.credentialId, name: `${item.label} (${item.tokenPrefix})` })}>Revoke</Button> : null}
                />
              ))}
            </Rows>
          )}
        </div>
      </div>
      <ConfirmDialog
        open={Boolean(pending)}
        title={`Revoke ${pending?.name ?? ""}?`}
        confirmLabel="Revoke"
        busy={revoke.isPending}
        onClose={() => setPending(null)}
        onConfirm={() => pending && revoke.mutate(pending, { onSuccess: () => setPending(null) })}
      >
        {pending?.kind === "device"
          ? "The Mac will need to pair again. Work stored on it isn’t affected."
          : "That browser will be signed out immediately."}
      </ConfirmDialog>
    </Section>
  );
}
