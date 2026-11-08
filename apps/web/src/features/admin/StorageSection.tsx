import { useState, type FormEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import type { StorageBinding, StorageRoot } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { Button, ConfirmDialog, Dialog, ErrorMessage, Field, Loading } from "../../ui";
import { ADMIN_POLL_MS, Row, Rows, Section, StateBadge, idempotencyKey, useAdminAction } from "./shared";
import styles from "./Admin.module.css";

export function StorageSection({ allowedMounts }: { allowedMounts: string[] | null }) {
  const client = useClient();
  const roots = useQuery({ queryKey: [...keys.admin, "roots"], queryFn: () => client.listAdminStorageRoots(), refetchInterval: ADMIN_POLL_MS });
  const bindings = useQuery({ queryKey: [...keys.admin, "bindings"], queryFn: () => client.listAdminStorageBindings(), refetchInterval: ADMIN_POLL_MS });
  const [adding, setAdding] = useState(false);
  const [connecting, setConnecting] = useState<string | null>(null);
  const items = roots.data?.items ?? [];

  return (
    <Section
      id="storage"
      title="Storage"
      description="Where original footage lives. Originals are only ever read."
      actions={<Button icon="plus" onClick={() => setAdding(true)}>Add storage</Button>}
    >
      {roots.isPending ? <Loading /> : null}
      <ErrorMessage error={roots.error ?? bindings.error} />
      {roots.data ? (
        <Rows empty="No storage yet.">
          {items.map((root) => (
            <RootRow
              key={root.id}
              root={root}
              binding={bindings.data?.items.find((item) => item.storageRootId === root.id) ?? null}
              onConnect={() => setConnecting(root.id)}
            />
          ))}
        </Rows>
      ) : null}
      <AddRootDialog open={adding} onClose={() => setAdding(false)} />
      <ConnectDialog
        root={items.find((root) => root.id === connecting) ?? null}
        allowedMounts={allowedMounts}
        onClose={() => setConnecting(null)}
      />
    </Section>
  );
}

function RootRow({ root, binding, onConnect }: { root: StorageRoot; binding: StorageBinding | null; onConnect: () => void }) {
  const client = useClient();
  const [renaming, setRenaming] = useState(false);
  const [name, setName] = useState(root.displayName);
  const [disabling, setDisabling] = useState(false);
  const rename = useAdminAction((displayName: string) => client.updateAdminStorageRoot(root.id, { displayName }));
  const scan = useAdminAction(() => client.requestAdminScan({ storageRootId: root.id, idempotencyKey: idempotencyKey() }));
  const verify = useAdminAction(() => client.verifyAdminStorageBinding(root.id));
  const confirm = useAdminAction((action: "confirm" | "reject") => client.confirmAdminStorageBinding(root.id, action));
  const disable = useAdminAction(() => client.updateAdminStorageRoot(root.id, { enabled: false, confirmDisable: true }));
  const error = rename.error ?? scan.error ?? verify.error ?? confirm.error ?? disable.error;

  const evidence = binding?.associationEvidence;
  const evidenceReady = Boolean(evidence && evidence.matchingRelativePaths > 0 && evidence.mismatches === 0);
  const confirmed = binding?.confirmation.status === "confirmed";
  const location = binding ? `${binding.localMountPath}${binding.rootSubpath ? `/${binding.rootSubpath}` : ""} (${binding.protocol}, ${binding.accessMode === "read_only" ? "read-only" : "read-write"})` : "Not connected on this server";
  const evidenceText = evidence ? `${evidence.matchingRelativePaths} matching files, ${evidence.mismatches} mismatches` : binding ? "Not verified yet" : null;

  function submitRename(event: FormEvent) {
    event.preventDefault();
    const value = name.trim();
    if (!value || value === root.displayName) { setRenaming(false); return; }
    rename.mutate(value, { onSuccess: () => setRenaming(false) });
  }

  return (
    <Row
      title={renaming ? (
        <form className={styles.inlineForm} onSubmit={submitRename}>
          <input aria-label="Storage name" value={name} autoFocus onChange={(event) => setName(event.target.value)} onKeyDown={(event) => { if (event.key === "Escape") setRenaming(false); }} />
          <Button size="sm" type="submit" variant="primary" busy={rename.isPending}>Save</Button>
          <Button size="sm" variant="ghost" onClick={() => setRenaming(false)}>Cancel</Button>
        </form>
      ) : root.displayName}
      meta={<>{location}{evidenceText ? ` · ${evidenceText}` : ""}</>}
      status={<>
        {!root.enabled ? <StateBadge state="disabled" /> : null}
        {binding ? <StateBadge state={binding.confirmation.status === "confirmed" ? binding.verificationStatus : binding.confirmation.status} /> : null}
      </>}
      actions={root.enabled ? <>
        {!renaming ? <Button size="sm" variant="ghost" onClick={() => { setName(root.displayName); setRenaming(true); }}>Rename</Button> : null}
        <Button size="sm" onClick={onConnect}>{binding ? "Change folder" : "Connect folder"}</Button>
        {binding && !confirmed ? <Button size="sm" busy={verify.isPending} onClick={() => verify.mutate()}>Verify</Button> : null}
        {evidenceReady && !confirmed ? <Button size="sm" variant="primary" busy={confirm.isPending} onClick={() => confirm.mutate("confirm")}>Confirm</Button> : null}
        {binding?.confirmation.status === "pending" ? <Button size="sm" variant="danger" onClick={() => confirm.mutate("reject")}>Reject</Button> : null}
        <Button size="sm" busy={scan.isPending} onClick={() => scan.mutate()}>Scan now</Button>
        <Button size="sm" variant="danger" onClick={() => setDisabling(true)}>Disable</Button>
      </> : null}
    >
      <ErrorMessage error={error} />
      <ConfirmDialog
        open={disabling}
        title={`Disable ${root.displayName}?`}
        confirmLabel="Disable"
        busy={disable.isPending}
        onClose={() => setDisabling(false)}
        onConfirm={() => disable.mutate(undefined, { onSuccess: () => setDisabling(false) })}
      >
        It stops being scanned. Everything already indexed from it is kept.
      </ConfirmDialog>
    </Row>
  );
}

function AddRootDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const client = useClient();
  const [name, setName] = useState("");
  const create = useAdminAction((displayName: string) => client.createAdminStorageRoot(displayName));
  function submit(event: FormEvent) {
    event.preventDefault();
    const value = name.trim();
    if (value) create.mutate(value, { onSuccess: () => { setName(""); onClose(); } });
  }
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title="Add storage"
      footer={<>
        <Button onClick={onClose}>Cancel</Button>
        <Button type="submit" form="add-root" variant="primary" busy={create.isPending} disabled={!name.trim()}>Add storage</Button>
      </>}
    >
      <form id="add-root" className={styles.formGrid} onSubmit={submit}>
        <Field label="Name" hint="A label for this archive, like “NAS footage”. You’ll connect a folder next.">
          {(id) => <input id={id} value={name} autoFocus onChange={(event) => setName(event.target.value)} placeholder="NAS footage" />}
        </Field>
        <ErrorMessage error={create.error} />
      </form>
    </Dialog>
  );
}

function ConnectDialog({ root, allowedMounts, onClose }: { root: StorageRoot | null; allowedMounts: string[] | null; onClose: () => void }) {
  const client = useClient();
  const [mountPath, setMountPath] = useState("");
  const [subpath, setSubpath] = useState("");
  const [protocol, setProtocol] = useState<StorageBinding["protocol"]>("local");
  const save = useAdminAction(() => client.upsertAdminStorageBinding(root!.id, {
    localMountPath: mountPath.trim(),
    ...(subpath.trim() ? { rootSubpath: subpath.trim() } : {}),
    protocol,
    accessMode: "read_only",
  }));
  const noMounts = allowedMounts !== null && allowedMounts.length === 0;

  function submit(event: FormEvent) {
    event.preventDefault();
    if (root && mountPath.trim()) save.mutate(undefined, { onSuccess: onClose });
  }

  return (
    <Dialog
      open={Boolean(root)}
      onClose={onClose}
      title={root ? `Connect a folder to ${root.displayName}` : ""}
      footer={<>
        <Button onClick={onClose}>Cancel</Button>
        {!noMounts ? <Button type="submit" form="connect-root" variant="primary" busy={save.isPending} disabled={!mountPath.trim()}>Connect</Button> : null}
      </>}
    >
      <form id="connect-root" className={styles.formGrid} onSubmit={submit}>
        {noMounts ? <p className={styles.none}>The server’s deployment config allows no footage mounts, so no folder can be connected from here.</p> : (
          <>
            <Field label="Mount" hint={allowedMounts?.length ? `Allowed: ${allowedMounts.join(", ")}` : undefined}>
              {(id) => allowedMounts?.length
                ? <select id={id} value={mountPath} onChange={(event) => setMountPath(event.target.value)}><option value="">Choose…</option>{allowedMounts.map((mount) => <option key={mount}>{mount}</option>)}</select>
                : <input id={id} value={mountPath} onChange={(event) => setMountPath(event.target.value)} placeholder="/Volumes/archive" />}
            </Field>
            <div className={styles.formRow}>
              <Field label="Subfolder" hint="Optional">
                {(id) => <input id={id} value={subpath} onChange={(event) => setSubpath(event.target.value)} placeholder="Trips/2026" />}
              </Field>
              <Field label="Protocol">
                {(id) => (
                  <select id={id} value={protocol} onChange={(event) => setProtocol(event.target.value as StorageBinding["protocol"])}>
                    <option value="local">Local disk</option><option value="smb">SMB</option><option value="nfs">NFS</option><option value="other">Other</option>
                  </select>
                )}
              </Field>
            </div>
            <p className={styles.rowMeta}>Connected read-only. Verify it next, then confirm once the files match.</p>
          </>
        )}
        <ErrorMessage error={save.error} />
      </form>
    </Dialog>
  );
}
