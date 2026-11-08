import { useState, type FormEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import type { ScanRun } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { formatDateTime, formatRelative } from "../../lib/format";
import { Button, Dialog, ErrorMessage, Field, Loading } from "../../ui";
import { ADMIN_POLL_MS, Row, Rows, Section, StateBadge, useAdminAction } from "./shared";
import styles from "./Admin.module.css";

export function ScansSection() {
  const client = useClient();
  const roots = useQuery({ queryKey: [...keys.admin, "roots"], queryFn: () => client.listAdminStorageRoots(), refetchInterval: ADMIN_POLL_MS });
  const scans = useQuery({ queryKey: [...keys.admin, "scans"], queryFn: () => client.listAdminScans(30), refetchInterval: ADMIN_POLL_MS });
  const schedules = useQuery({ queryKey: [...keys.admin, "schedules"], queryFn: () => client.listAdminScanSchedules(), refetchInterval: ADMIN_POLL_MS });
  const toggle = useAdminAction(({ scheduleId, enabled }: { scheduleId: string; enabled: boolean }) => client.updateAdminScanSchedule(scheduleId, { enabled }));
  const [adding, setAdding] = useState(false);
  const rootName = (id: string) => roots.data?.items.find((root) => root.id === id)?.displayName ?? id;
  const enabledRoots = roots.data?.items.filter((root) => root.enabled) ?? [];

  return (
    <Section
      id="scans"
      title="Scans"
      description="Scans index new and changed files in storage."
      actions={<Button icon="plus" disabled={!enabledRoots.length} onClick={() => setAdding(true)}>Schedule</Button>}
    >
      <ErrorMessage error={scans.error ?? schedules.error ?? toggle.error} />
      <div className={styles.columns}>
        <div>
          <h3>Recent</h3>
          {scans.isPending ? <Loading /> : (
            <Rows empty="No scans yet.">
              {(scans.data?.items ?? []).map((scan) => <ScanRow key={scan.scanId} scan={scan} rootName={rootName(scan.storageRootId)} />)}
            </Rows>
          )}
        </div>
        <div>
          <h3>Schedules</h3>
          {schedules.isPending ? <Loading /> : (
            <Rows empty="No scheduled scans.">
              {(schedules.data?.items ?? []).map((schedule) => (
                <Row
                  key={schedule.scheduleId}
                  title={`${rootName(schedule.storageRootId)} · daily at ${schedule.localTime}`}
                  meta={schedule.enabled ? `${schedule.timezone} · next ${formatDateTime(schedule.nextRunAt)}` : `${schedule.timezone} · paused`}
                  actions={<Button size="sm" busy={toggle.isPending && toggle.variables?.scheduleId === schedule.scheduleId} onClick={() => toggle.mutate({ scheduleId: schedule.scheduleId, enabled: !schedule.enabled })}>{schedule.enabled ? "Pause" : "Resume"}</Button>}
                />
              ))}
            </Rows>
          )}
        </div>
      </div>
      <ScheduleDialog open={adding} roots={enabledRoots.map((root) => ({ id: root.id, name: root.displayName }))} onClose={() => setAdding(false)} />
    </Section>
  );
}

function ScanRow({ scan, rootName }: { scan: ScanRun; rootName: string }) {
  const client = useClient();
  const diagnostics = useQuery({
    queryKey: [...keys.admin, "scan-diagnostics", scan.scanId],
    queryFn: () => client.listAdminScanDiagnostics(scan.scanId, 20),
    enabled: scan.failedFiles > 0,
  });
  const counts = [
    `${scan.completedFiles.toLocaleString()} indexed`,
    scan.queuedFiles ? `${scan.queuedFiles.toLocaleString()} queued` : null,
    scan.failedFiles ? `${scan.failedFiles.toLocaleString()} failed` : null,
    `${scan.visitedEntries.toLocaleString()} checked`,
  ].filter(Boolean).join(" · ");
  return (
    <Row
      title={rootName}
      meta={`${scan.source === "schedule" ? "Scheduled" : "Manual"} · ${formatRelative(scan.startedAt ?? scan.createdAt)} · ${counts}`}
      status={<StateBadge state={scan.state} />}
    >
      {diagnostics.data?.items.length ? (
        <ul className={styles.diagnostics}>
          {diagnostics.data.items.map((item) => (
            <li key={item.diagnosticId}><code>{item.code}</code>{item.stage} · {item.safePathIdentity}{item.retryable ? " · will retry" : ""}</li>
          ))}
        </ul>
      ) : null}
    </Row>
  );
}

function ScheduleDialog({ open, roots, onClose }: { open: boolean; roots: Array<{ id: string; name: string }>; onClose: () => void }) {
  const client = useClient();
  const [rootId, setRootId] = useState("");
  const [time, setTime] = useState("03:00");
  const [timezone, setTimezone] = useState(() => Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC");
  const selectedRoot = rootId || roots[0]?.id || "";
  const create = useAdminAction(() => client.createAdminScanSchedule({ storageRootId: selectedRoot, enabled: true, localTime: time, timezone }));

  function submit(event: FormEvent) {
    event.preventDefault();
    if (selectedRoot) create.mutate(undefined, { onSuccess: onClose });
  }

  return (
    <Dialog
      open={open}
      onClose={onClose}
      title="Schedule a daily scan"
      footer={<>
        <Button onClick={onClose}>Cancel</Button>
        <Button type="submit" form="add-schedule" variant="primary" busy={create.isPending} disabled={!selectedRoot}>Add schedule</Button>
      </>}
    >
      <form id="add-schedule" className={styles.formGrid} onSubmit={submit}>
        <Field label="Storage">
          {(id) => <select id={id} value={selectedRoot} onChange={(event) => setRootId(event.target.value)}>{roots.map((root) => <option key={root.id} value={root.id}>{root.name}</option>)}</select>}
        </Field>
        <div className={styles.formRow}>
          <Field label="Time">{(id) => <input id={id} type="time" value={time} onChange={(event) => setTime(event.target.value)} />}</Field>
          <Field label="Time zone">{(id) => <input id={id} value={timezone} onChange={(event) => setTimezone(event.target.value)} />}</Field>
        </div>
        <ErrorMessage error={create.error} />
      </form>
    </Dialog>
  );
}
