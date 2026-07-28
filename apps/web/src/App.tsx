import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type ReactNode } from "react";
import {
  ProjectClientError,
  type AddToProjectPreview,
  type AdminAuditEvent,
  type AdminJob,
  type AdminOverview,
  type BrowserProjectClient,
  type LibrarySearchItem,
  type LibrarySort,
  type ProjectAssetItem,
  type ProjectReviewAsset,
  type ReviewMutationResult,
  type ProjectSummary,
  type ProjectSyncOverview,
  type ScanRun,
  type ScanFileDiagnostic,
  type ScanSchedule,
  type ServerStatus,
  type SessionState,
  type StorageBinding,
  type StorageRoot,
} from "@rcdvlog/project-client";
import { StatePanel, StatusBadge } from "@rcdvlog/review-ui";

const PROJECT_REFRESH_INTERVAL_MS = 5_000;

type ShellState =
  | { kind: "loading" }
  | { kind: "login"; status: ServerStatus }
  | { kind: "ready"; status: ServerStatus; session: SessionState }
  | { kind: "error"; error: ProjectClientError };

export default function App({ client }: { client: BrowserProjectClient }) {
  const [state, setState] = useState<ShellState>({ kind: "loading" });

  async function bootstrap() {
    setState({ kind: "loading" });
    try {
      const status = await client.getServerStatus();
      const session = await client.getSession();
      setState(session.authenticated ? { kind: "ready", status, session } : { kind: "login", status });
    } catch (error) {
      setState({ kind: "error", error: normalizeError(error) });
    }
  }

  useEffect(() => { void bootstrap(); }, [client]);

  if (state.kind === "loading") {
    return <StateScreen title="Opening the review desk" detail="Checking the private server and your session…" busy />;
  }
  if (state.kind === "error") {
    return <ErrorScreen error={state.error} retry={() => void bootstrap()} />;
  }
  if (state.kind === "login") {
    return <LoginScreen client={client} status={state.status} complete={bootstrap} />;
  }
  return <AuthenticatedShell client={client} status={state.status} session={state.session} />;
}

function LoginScreen({
  client,
  status,
  complete,
}: {
  client: BrowserProjectClient;
  status: ServerStatus;
  complete: () => Promise<void>;
}) {
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ProjectClientError | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!code.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await client.loginWithBootstrapCode(code.trim());
      await complete();
    } catch (reason) {
      setError(normalizeError(reason));
      setBusy(false);
    }
  }

  return (
    <main className="login-layout">
      <FilmRail />
      <section className="login-copy" aria-labelledby="login-title">
        <div className="brand"><span>RCD</span> VlogCutter</div>
        <p className="eyebrow">Private review server</p>
        <h1 id="login-title">Your footage stays close.<br />Your review desk travels.</h1>
        <p className="lede">Use a one-time bootstrap code from the server to open this browser session.</p>
        <StatusSummary status={status} />
      </section>
      <section className="login-card" aria-label="Sign in">
        <p className="card-index">Session / Chrome</p>
        <h2>Open review desk</h2>
        <form onSubmit={submit}>
          <label htmlFor="bootstrap-code">Bootstrap code</label>
          <input
            id="bootstrap-code"
            value={code}
            onChange={(event) => setCode(event.target.value)}
            autoComplete="one-time-code"
            spellCheck={false}
            placeholder="Enter the server code"
            disabled={busy}
          />
          {error && <InlineError error={error} />}
          <button disabled={busy || !code.trim()}>{busy ? "Opening…" : "Open review desk"}</button>
        </form>
        <p className="help">Codes expire quickly and can be used once. The browser session remains on this server origin.</p>
      </section>
    </main>
  );
}

function AuthenticatedShell({ client, status, session }: { client: BrowserProjectClient; status: ServerStatus; session: SessionState }) {
  const [location, setLocation] = useState(() => `${window.location.pathname}${window.location.search}`);
  const url = useMemo(() => new URL(location, window.location.origin), [location]);
  const path = url.pathname;
  const routes = [
    { path: "/", label: "Overview" },
    { path: "/library", label: "Library" },
    { path: "/projects", label: "Projects" },
    { path: "/admin", label: "Administration" },
  ];

  function navigate(next: string) {
    window.history.pushState({}, "", next);
    setLocation(next);
  }

  useEffect(() => {
    const pop = () => setLocation(`${window.location.pathname}${window.location.search}`);
    window.addEventListener("popstate", pop);
    return () => window.removeEventListener("popstate", pop);
  }, []);

  return (
    <div className="shell">
      <FilmRail compact />
      <header className="shell-header">
        <a href="/" className="brand" onClick={(event) => { event.preventDefault(); navigate("/"); }}><span>RCD</span> VlogCutter</a>
        <StatusSummary status={status} compact />
      </header>
      <nav className="shell-nav" aria-label="Primary">
        {routes.map((route) => (
          <a
            key={route.path}
            href={route.path}
            aria-current={path === route.path ? "page" : undefined}
            onClick={(event) => { event.preventDefault(); navigate(route.path); }}
          >
            {route.label}
          </a>
        ))}
      </nav>
      <main className="shell-content">
        {path === "/library" ? (
          <LibraryView client={client} url={url} navigate={navigate} />
        ) : path.startsWith("/projects/") ? (
          <ProjectAssetsView client={client} projectId={decodeURIComponent(path.slice("/projects/".length))} />
        ) : path === "/projects" ? (
          <ProjectsView client={client} navigate={navigate} />
        ) : path === "/admin" ? (
          <AdminView client={client} initialSession={session} />
        ) : <Overview status={status} session={session} />}
      </main>
    </div>
  );
}

function AdminView({ client, initialSession }: { client: BrowserProjectClient; initialSession: SessionState }) {
  const [overview, setOverview] = useState<AdminOverview | null>(null);
  const [roots, setRoots] = useState<StorageRoot[]>([]);
  const [bindings, setBindings] = useState<StorageBinding[]>([]);
  const [scans, setScans] = useState<ScanRun[]>([]);
  const [scanDiagnostics, setScanDiagnostics] = useState<Record<string, ScanFileDiagnostic[]>>({});
  const [schedules, setSchedules] = useState<ScanSchedule[]>([]);
  const [jobs, setJobs] = useState<AdminJob[]>([]);
  const [sync, setSync] = useState<ProjectSyncOverview[]>([]);
  const [sessions, setSessions] = useState(initialSession.sessions);
  const [devices, setDevices] = useState<Awaited<ReturnType<BrowserProjectClient["listDevices"]>>>([]);
  const [audit, setAudit] = useState<AdminAuditEvent[]>([]);
  const [error, setError] = useState<ProjectClientError | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [rootName, setRootName] = useState("");
  const [bindingRoot, setBindingRoot] = useState("");
  const [mountPath, setMountPath] = useState("");
  const [rootSubpath, setRootSubpath] = useState("");
  const [bindingProtocol, setBindingProtocol] = useState<StorageBinding["protocol"]>("local");
  const [scheduleRoot, setScheduleRoot] = useState("");
  const [scheduleTime, setScheduleTime] = useState("03:00");
  const [scheduleTimezone, setScheduleTimezone] = useState(Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC");
  const [batchIds, setBatchIds] = useState("");
  const [batchMessage, setBatchMessage] = useState("");

  const load = useCallback(async () => {
    try {
      const [nextOverview, nextRoots, nextBindings, nextScans, nextSchedules, nextJobs, nextSync, nextSession, nextDevices, nextAudit] = await Promise.all([
        client.getAdminOverview(), client.listAdminStorageRoots(), client.listAdminStorageBindings(), client.listAdminScans(30),
        client.listAdminScanSchedules(), client.listAdminJobs(50), client.listAdminSync(), client.getSession(), client.listDevices(), client.listAdminAudit(30),
      ]);
      if (!nextSession.authenticated) {
        window.location.reload();
        return;
      }
      const diagnostics = await Promise.all(nextScans.items.filter((scan) => scan.failedFiles > 0).map(async (scan) => [scan.scanId, (await client.listAdminScanDiagnostics(scan.scanId, 20)).items] as const));
      setOverview(nextOverview); setRoots(nextRoots.items); setBindings(nextBindings.items); setScans(nextScans.items); setSchedules(nextSchedules.items);
      setScanDiagnostics(Object.fromEntries(diagnostics));
      setJobs(nextJobs.items); setSync(nextSync.items); setSessions(nextSession.sessions); setDevices(nextDevices); setAudit(nextAudit.items);
      setScheduleRoot((current) => current || nextRoots.items.find((root) => root.enabled)?.id || ""); setError(null);
      setBindingRoot((current) => current || nextRoots.items.find((root) => root.enabled)?.id || "");
    } catch (reason) { setError(normalizeError(reason)); }
  }, [client]);

  useEffect(() => { void load(); const timer = window.setInterval(() => void load(), 15_000); return () => window.clearInterval(timer); }, [load]);
  useEffect(() => {
    if (typeof WebSocket === "undefined") return;
    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    const socket = new WebSocket(`${protocol}//${window.location.host}/api/v1/ws/events`);
    socket.addEventListener("open", () => socket.send(JSON.stringify({ type: "subscribe", projectId: "__admin__" })));
    socket.addEventListener("message", () => void load());
    return () => socket.close();
  }, [load]);

  async function action(key: string, operation: () => Promise<unknown>) {
    setBusy(key); setError(null);
    try { await operation(); await load(); } catch (reason) { setError(normalizeError(reason)); } finally { setBusy(null); }
  }

  return <section className="workspace-page admin-page">
    <div className="page-heading"><div><p className="eyebrow">Home node / controlled operations</p><h1>Administration</h1></div>{overview ? <StatusBadge label={overview.readiness.status === "ready" ? "All prerequisites ready" : "Attention required"} tone={overview.readiness.status === "ready" ? "positive" : "warning"} /> : null}</div>
    {error ? <InlineError error={error} /> : null}
    {overview ? <div className="admin-vitals">
      <article><span>Node</span><strong>{overview.nodeId}</strong><small>server {overview.applicationVersion} · schema {overview.controlSchemaVersion}</small></article>
      <article><span>Derived capacity</span><strong>{formatBytes(overview.derivedAvailableBytes)}</strong><small>{overview.config.derivedDir}</small></article>
      <article><span>Credentials</span><strong>{overview.auth.activeSessions + overview.auth.activeDeviceCredentials}</strong><small>{overview.auth.activeSessions} sessions · {overview.auth.activeDeviceCredentials} Macs</small></article>
      <article><span>Prerequisites</span><strong>{overview.readiness.checks.filter((check) => check.status === "ok").length}/{overview.readiness.checks.length}</strong><small>{overview.readiness.checks.filter((check) => check.status !== "ok").map((check) => check.code).join(", ") || "All checks pass"}</small></article>
    </div> : <p className="loading-line">Loading durable server state…</p>}

    <AdminSection index="01" title="Storage roots & bindings">
      <form className="admin-inline-form" onSubmit={(event) => { event.preventDefault(); const name = rootName.trim(); if (name) void action("create-root", () => client.createAdminStorageRoot(name).then(() => setRootName(""))); }}><label>Logical root name<input value={rootName} onChange={(event) => setRootName(event.target.value)} placeholder="Expo archive" /></label><button disabled={!rootName.trim() || busy === "create-root"}>Register root</button></form>
      <div className="admin-table">{roots.map((root) => { const binding = bindings.find((item) => item.storageRootId === root.id); const evidenceReady = !!binding?.associationEvidence && binding.associationEvidence.matchingRelativePaths > 0 && binding.associationEvidence.mismatches === 0; return <article key={root.id}><div><strong>{root.displayName}</strong><small>{root.id} · {root.immutableOriginal ? "immutable originals" : "writable root"}</small></div><div><StatusBadge label={root.enabled ? "Enabled" : "Disabled"} tone={root.enabled ? "positive" : "warning"} /><small>{binding ? `${binding.confirmation.status} · ${binding.verificationStatus} · ${binding.accessMode} ${binding.protocol} · ${binding.localMountPath}${binding.rootSubpath ? ` / ${binding.rootSubpath}` : ""}${binding.associationEvidence ? ` · ${binding.associationEvidence.matchingRelativePaths} paths / ${binding.associationEvidence.mismatches} mismatches` : " · evidence not checked"}` : "No server binding"}</small></div><div className="row-actions"><button className="secondary" disabled={busy !== null} onClick={() => { const displayName = window.prompt("Logical storage-root display name", root.displayName)?.trim(); if (displayName && displayName !== root.displayName) void action(`rename-${root.id}`, () => client.updateAdminStorageRoot(root.id, { displayName })); }}>Rename</button><button className="secondary" disabled={!root.enabled || busy !== null} onClick={() => void action(`scan-${root.id}`, () => client.requestAdminScan({ storageRootId: root.id, idempotencyKey: `web-${Date.now()}` }))}>Scan now</button>{binding && binding.confirmation.status !== "confirmed" ? <button className="secondary" disabled={busy !== null} onClick={() => void action(`verify-${root.id}`, () => client.verifyAdminStorageBinding(root.id))}>Verify</button> : null}{evidenceReady && binding?.confirmation.status !== "confirmed" ? <button disabled={busy !== null} onClick={() => void action(`confirm-${root.id}`, () => client.confirmAdminStorageBinding(root.id, "confirm"))}>Confirm</button> : null}{binding && binding.confirmation.status === "pending" ? <button className="danger-button" disabled={busy !== null} onClick={() => void action(`reject-${root.id}`, () => client.confirmAdminStorageBinding(root.id, "reject"))}>Reject</button> : null}{root.enabled ? <button className="danger-button" disabled={busy !== null} onClick={() => { if (window.confirm(`Disable ${root.displayName}? Historical records remain intact.`)) void action(`disable-${root.id}`, () => client.updateAdminStorageRoot(root.id, { enabled: false, confirmDisable: true })); }}>Disable</button> : null}</div></article>; })}</div>
      {overview?.config.allowedOriginalMounts.length ? <p className="admin-note">Bindings are limited to deployment-approved mounts: {overview.config.allowedOriginalMounts.join(" · ")}. Association evidence must be present before confirmation.</p> : <p className="admin-note warning">No original mounts are allowed by deployment configuration. The web UI cannot register arbitrary paths.</p>}
      <form className="admin-inline-form binding-form" onSubmit={(event) => { event.preventDefault(); if (!bindingRoot || !mountPath.trim()) return; void action("binding", () => client.upsertAdminStorageBinding(bindingRoot, { localMountPath: mountPath.trim(), ...(rootSubpath.trim() ? { rootSubpath: rootSubpath.trim() } : {}), protocol: bindingProtocol, accessMode: "read_only" })); }}><label>Logical root<select value={bindingRoot} onChange={(event) => setBindingRoot(event.target.value)}>{roots.filter((root) => root.enabled).map((root) => <option key={root.id} value={root.id}>{root.displayName}</option>)}</select></label><label>Approved local mount<input value={mountPath} onChange={(event) => setMountPath(event.target.value)} placeholder={overview?.config.allowedOriginalMounts[0] ?? "/Volumes/archive"} /></label><label>Optional root subpath<input value={rootSubpath} onChange={(event) => setRootSubpath(event.target.value)} placeholder="Trips/2026" /></label><label>Protocol<select value={bindingProtocol} onChange={(event) => setBindingProtocol(event.target.value as StorageBinding["protocol"])}><option value="local">Local</option><option value="smb">SMB</option><option value="nfs">NFS</option><option value="other">Other</option></select></label><button disabled={!bindingRoot || !mountPath.trim() || busy !== null}>Configure pending binding</button></form>
    </AdminSection>

    <AdminSection index="02" title="Scans & schedules">
      <form className="admin-inline-form schedule-form" onSubmit={(event) => { event.preventDefault(); if (scheduleRoot) void action("schedule", () => client.createAdminScanSchedule({ storageRootId: scheduleRoot, enabled: true, localTime: scheduleTime, timezone: scheduleTimezone })); }}><label>Root<select value={scheduleRoot} onChange={(event) => setScheduleRoot(event.target.value)}>{roots.filter((root) => root.enabled).map((root) => <option key={root.id} value={root.id}>{root.displayName}</option>)}</select></label><label>Local time<input type="time" value={scheduleTime} onChange={(event) => setScheduleTime(event.target.value)} /></label><label>IANA timezone<input value={scheduleTimezone} onChange={(event) => setScheduleTimezone(event.target.value)} /></label><button disabled={!scheduleRoot || busy !== null}>Add schedule</button></form>
      <div className="admin-split"><div><h3>Recent scans</h3>{scans.map((scan) => <div className="scan-entry" key={scan.scanId}><p className="admin-line"><span>{scan.storageRootId}<small>{scan.source} · {scan.visitedEntries} visited · {scan.queuedFiles} queued · {scan.completedFiles} completed · {scan.failedFiles} failed</small></span><StatusBadge label={scan.state} tone={scan.state === "failed" ? "critical" : scan.state === "succeeded" ? "positive" : "warning"} /></p>{scanDiagnostics[scan.scanId]?.map((diagnostic) => <p className="scan-diagnostic" key={diagnostic.diagnosticId}><code>{diagnostic.code}</code><span>{diagnostic.stage} · {diagnostic.safePathIdentity}{diagnostic.retryable ? " · retryable" : ""}</span></p>)}</div>)}</div><div><h3>Schedules</h3>{schedules.map((schedule) => <p className="admin-line" key={schedule.scheduleId}><span>{schedule.localTime} · {schedule.timezone}<small>next {schedule.nextRunAt}</small></span><button className="secondary" onClick={() => void action(`schedule-${schedule.scheduleId}`, () => client.updateAdminScanSchedule(schedule.scheduleId, { enabled: !schedule.enabled }))}>{schedule.enabled ? "Pause" : "Enable"}</button></p>)}</div></div>
    </AdminSection>

    <AdminSection index="03" title="Durable jobs & representation batches">
      <p className="admin-note">Execution node: {overview?.nodeId ?? "loading"}. Originals are read only; outputs are written under the configured derived root.</p>
      <form className="batch-form" onSubmit={(event) => { event.preventDefault(); const mediaObjectIds = batchIds.split(/[\s,]+/).filter(Boolean); if (!mediaObjectIds.length) return; void action("batch", async () => { const result = await client.requestAdminRepresentationBatch({ mediaObjectIds, kind: "proxy", idempotencyKey: `web-${Date.now()}` }); setBatchMessage(`${result.queuedCount} queued · ${result.reusedCount} reused · ${result.succeededCount} succeeded · ${result.failedCount} failed · ${result.skippedCount} skipped${result.skipped.length ? ` (${result.skipped.map((item) => item.reason).join(", ")})` : ""}`); }); }}><label>Full-hash-verified media IDs<textarea value={batchIds} onChange={(event) => setBatchIds(event.target.value)} placeholder="media_… one per line" /></label><button disabled={!batchIds.trim() || busy !== null}>Request proxies</button>{batchMessage ? <p>{batchMessage}</p> : null}</form>
      <div className="admin-table compact">{jobs.map((job) => <article key={job.definition.jobId}><div><strong>{job.definition.jobType}</strong><small>{job.definition.jobId} · {job.definition.targetNodeId}</small></div><div className="job-progress"><span style={{ width: `${job.totalItems ? Math.min(100, (job.completedItems / job.totalItems) * 100) : 0}%` }} /><small>{job.completedItems}/{job.totalItems} items · {job.failedItems} failed · attempt {job.attemptCount} · {job.estimate?.workerCount ?? 1} worker · ETA {job.estimate?.estimatedRemainingSeconds == null ? "calculating" : formatEta(job.estimate.estimatedRemainingSeconds)}</small></div><StatusBadge label={job.state} tone={job.state === "failed" ? "critical" : job.state === "succeeded" ? "positive" : "warning"} /><div className="row-actions">{["queued", "running", "retry_wait"].includes(job.state) ? <button className="danger-button" onClick={() => void action(`cancel-${job.definition.jobId}`, () => client.cancelAdminJob(job.definition.jobId))}>Cancel</button> : null}{job.state === "failed" ? <button className="secondary" onClick={() => void action(`retry-${job.definition.jobId}`, () => client.retryAdminJob(job.definition.jobId))}>Retry if eligible</button> : null}</div></article>)}</div>
    </AdminSection>

    <AdminSection index="04" title="Project synchronization">
      <div className="admin-table compact">{sync.map((project) => <article key={project.projectId}><div><strong>{project.projectId}</strong><small>{project.originNodeId ?? "origin unknown"} → {project.serverNodeId} · endpoint {project.endpoint ?? "not recorded"}</small></div><div><span>{project.operationCount} stored operations · cursor {project.latestLocalSequence}</span><small>{project.nodeObservationCount} node observations · {project.connectivity} · {project.pendingProjectionRepairs} pending repairs · last {project.lastSuccessAt}</small></div><StatusBadge label={project.unresolvedConflictCount ? `${project.unresolvedConflictCount} conflicts · ${project.recommendations.join(", ")}` : project.compatibility} tone={project.unresolvedConflictCount ? "warning" : "positive"} /></article>)}</div>
    </AdminSection>

    <AdminSection index="05" title="Sessions, paired Macs & audit">
      <div className="admin-split"><div><h3>Browser sessions</h3>{sessions.map((item) => <p className="admin-line" key={item.sessionId}><span>{item.hostname}<small>{item.displayPrefix} · last {item.lastUsedAt}</small></span>{!item.revoked ? <button className="danger-button" onClick={() => { if (window.confirm(`Revoke browser session ${item.displayPrefix} on ${item.hostname}?`)) void action(`session-${item.sessionId}`, () => client.revokeSession(item.sessionId)); }}>Revoke</button> : <span>Revoked</span>}</p>)}</div><div><h3>Paired desktops</h3>{devices.map((item) => <p className="admin-line" key={item.credentialId}><span>{item.label}<small>{item.tokenPrefix} · last {item.lastUsedAt}</small></span>{!item.revoked ? <button className="danger-button" onClick={() => { if (window.confirm(`Revoke ${item.label} (${item.tokenPrefix})? Local desktop work remains intact.`)) void action(`device-${item.credentialId}`, () => client.revokeDevice(item.credentialId)); }}>Revoke</button> : <span>Revoked</span>}</p>)}</div></div>
      <h3>Recent administrative audit</h3><div className="audit-list">{audit.map((event) => <p key={event.eventSequence}><b>{event.action}</b><span>{event.actorType} · {event.actorId}</span><small>{event.occurredAt} · request {event.requestId}</small></p>)}</div>
    </AdminSection>
  </section>;
}

function AdminSection({ index, title, children }: { index: string; title: string; children: ReactNode }) {
  return <section className="admin-section"><header><span>{index}</span><h2>{title}</h2></header><div>{children}</div></section>;
}

function Overview({ status, session }: { status: ServerStatus; session: SessionState }) {
  return <>
    <p className="eyebrow">Server workspace</p>
    <h1>Review desk ready</h1>
    <p className="lede">Browse the indexed archive or open a replicated project. Originals remain untouched in their registered storage roots.</p>
    <div className="foundation-grid">
          <article>
            <span className="card-index">Connection</span>
            <h2>{status.version.nodeId}</h2>
            <p>Server {status.version.applicationVersion} · protocol {status.version.protocolVersion.major}.{status.version.protocolVersion.minor}</p>
          </article>
          <article>
            <span className="card-index">Browser session</span>
            <h2>{session.sessions.length} active {session.sessions.length === 1 ? "session" : "sessions"}</h2>
            <p>Authenticated with a same-origin, HTTP-only session.</p>
          </article>
          <article className="empty-card">
            <span className="card-index">Queue</span>
            <h2>Nothing needs attention</h2>
            <p>Jobs and storage status will appear here when administration lands.</p>
          </article>
    </div>
  </>;
}

function LibraryView({ client, url, navigate }: { client: BrowserProjectClient; url: URL; navigate: (path: string) => void }) {
  const query = url.searchParams.get("q") ?? "";
  const sort = (url.searchParams.get("sort") as LibrarySort | null) ?? "createdDesc";
  const verification = url.searchParams.get("verification") ?? "";
  const [items, setItems] = useState<LibrarySearchItem[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<ProjectClientError | null>(null);
  const [addOpen, setAddOpen] = useState(false);

  async function load(after?: string, append = false) {
    setLoading(true); setError(null);
    try {
      const page = await client.listLibraryMedia({
        limit: 48,
        sort,
        ...(query ? { filenamePrefix: query } : {}),
        ...(verification ? { verificationState: verification as "verified" | "provisional" | "unverified" | "mismatch" } : {}),
        ...(after ? { after } : {}),
      });
      setItems((current) => append ? [...current, ...page.items] : page.items);
      setCursor(page.nextCursor);
    } catch (reason) { setError(normalizeError(reason)); }
    finally { setLoading(false); }
  }

  useEffect(() => { void load(); }, [client, query, sort, verification]);

  function updateQuery(values: Record<string, string>) {
    const next = new URLSearchParams(url.searchParams);
    for (const [key, value] of Object.entries(values)) value ? next.set(key, value) : next.delete(key);
    navigate(`/library${next.size ? `?${next}` : ""}`);
  }

  function toggle(id: string) {
    setSelected((current) => { const next = new Set(current); next.has(id) ? next.delete(id) : next.add(id); return next; });
  }

  return <section className="workspace-page">
    <div className="page-heading">
      <div><p className="eyebrow">Global archive / indexed</p><h1>Library</h1></div>
      <div className="selection-summary"><strong>{selected.size}</strong><span>selected</span><button disabled={!selected.size} onClick={() => setAddOpen(true)}>Add to project</button></div>
    </div>
    <form className="filter-bar" onSubmit={(event) => { event.preventDefault(); const data = new FormData(event.currentTarget); const nextQuery = String(data.get("q") ?? ""); updateQuery({ q: nextQuery, verification: nextQuery ? "" : verification, sort: nextQuery ? "filenameAsc" : sort }); }}>
      <label>Filename prefix<input name="q" defaultValue={query} placeholder="C0103" /></label>
      <label>Sort<select value={sort} onChange={(event) => { const nextSort = event.target.value; updateQuery({ sort: nextSort, q: nextSort === "filenameAsc" ? query : "", verification: nextSort === "createdDesc" ? verification : "" }); }}><option value="createdDesc">Newest first</option><option value="filenameAsc">Filename</option><option value="sizeDesc">Largest first</option></select></label>
      <label>Identity<select value={verification} onChange={(event) => updateQuery({ verification: event.target.value, q: "", sort: "createdDesc" })}><option value="">Any state</option><option value="verified">Verified</option><option value="provisional">Provisional</option><option value="unverified">Unverified</option><option value="mismatch">Mismatch</option></select></label>
      <button>Search</button>
    </form>
    {error ? <InlineError error={error} /> : null}
    {!loading && !items.length ? <StatePanel title="No footage in this view"><p>Change the filename or identity filter, or run a storage scan from administration.</p></StatePanel> : null}
    <div className="contact-sheet" aria-busy={loading}>
      {items.map((item, index) => <LibraryCard key={item.media.id} item={item} index={index} selected={selected.has(item.media.id)} toggle={() => toggle(item.media.id)} />)}
    </div>
    {loading ? <p className="loading-line">Reading the indexed page…</p> : cursor ? <button className="load-more" onClick={() => void load(cursor, true)}>Load next page</button> : null}
    {addOpen ? <AddToProjectDialog client={client} mediaObjectIds={[...selected]} close={() => setAddOpen(false)} complete={() => { setSelected(new Set()); setAddOpen(false); void load(); }} /> : null}
  </section>;
}

function LibraryCard({ item, index, selected, toggle }: { item: LibrarySearchItem; index: number; selected: boolean; toggle: () => void }) {
  const camera = [item.cameraMake, item.cameraModel].filter(Boolean).join(" ") || "Camera unknown";
  const verified = item.media.verificationState === "verified";
  return <article className={`media-card${selected ? " selected" : ""}`}>
    <button className="frame" onClick={toggle} aria-pressed={selected} aria-label={`${selected ? "Deselect" : "Select"} ${item.filename}`}>
      {item.thumbnailRepresentationId ? <img src={`/api/v1/media/${encodeURIComponent(item.thumbnailRepresentationId)}/content`} alt="" /> : null}
      <span className="frame-number">{String(index + 1).padStart(3, "0")}</span>
      <span className="frame-name">{item.filename}</span>
      <span className="frame-mark">{selected ? "Selected" : item.thumbnailRepresentationId ? "Quality 1" : verified ? "Thumbnail not generated" : "Identity required"}</span>
    </button>
    <div className="media-meta"><strong>{item.filename}</strong><span>{camera}</span><StatusBadge label={item.media.verificationState} tone={verified ? "positive" : item.media.verificationState === "mismatch" ? "critical" : "warning"} /><span>{availabilityLabel(item.availability)}</span><span>{formatBytes(item.media.sizeBytes)}</span></div>
  </article>;
}

function ProjectsView({ client, navigate }: { client: BrowserProjectClient; navigate: (path: string) => void }) {
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [error, setError] = useState<ProjectClientError | null>(null);
  useEffect(() => { client.listProjects().then((page) => setProjects(page.items)).catch((reason) => setError(normalizeError(reason))); }, [client]);
  return <section className="workspace-page"><div className="page-heading"><div><p className="eyebrow">Replicated catalogs</p><h1>Projects</h1></div></div>
    {error ? <InlineError error={error} /> : null}
    <div className="project-list">{projects.map((project) => <button key={project.projectId} onClick={() => navigate(`/projects/${encodeURIComponent(project.projectId)}`)}><span><strong>{project.projectId}</strong><small>{project.state}</small></span><b>{project.assetCount} clips</b></button>)}</div>
    {!error && !projects.length ? <StatePanel title="No replicated projects"><p>Pair and adopt an existing desktop project before adding archive footage.</p></StatePanel> : null}
  </section>;
}

function ProjectAssetsView({ client, projectId }: { client: BrowserProjectClient; projectId: string }) {
  const [assets, setAssets] = useState<ProjectAssetItem[]>([]);
  const [search, setSearch] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [review, setReview] = useState<ProjectReviewAsset | null>(null);
  const [noteDraft, setNoteDraft] = useState("");
  const [labelDraft, setLabelDraft] = useState("");
  const [syncState, setSyncState] = useState<"synced" | "saving" | "conflict">("synced");
  const [staleNote, setStaleNote] = useState(false);
  const [playbackError, setPlaybackError] = useState<"session" | "media" | null>(null);
  const [error, setError] = useState<ProjectClientError | null>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const labelRef = useRef<HTMLInputElement>(null);
  const noteRef = useRef<HTMLTextAreaElement>(null);

  const loadAssets = useCallback(async () => {
    try {
      const page = await client.listProjectAssets(projectId, 100, undefined, search || undefined);
      setAssets(page.items);
      setSelectedId((current) => current && page.items.some((asset) => asset.projectAssetId === current) ? current : page.items[0]?.projectAssetId ?? null);
      setError(null);
    } catch (reason) { setError(normalizeError(reason)); }
  }, [client, projectId, search]);

  const loadReview = useCallback(async (assetId: string) => {
    try {
      const asset = await client.getProjectReviewAsset(projectId, assetId);
      setReview(asset);
      setNoteDraft(asset.activeNote ?? "");
      setSyncState(asset.unresolvedConflictCount ? "conflict" : "synced");
      setPlaybackError(null);
      setError(null);
    } catch (reason) { setError(normalizeError(reason)); }
  }, [client, projectId]);

  useEffect(() => { void loadAssets(); }, [loadAssets]);
  useEffect(() => {
    if (selectedId) void loadReview(selectedId);
    else setReview(null);
  }, [loadReview, selectedId]);

  useEffect(() => {
    if (!selectedId || typeof WebSocket === "undefined") return;
    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    const socket = new WebSocket(`${protocol}//${window.location.host}/api/v1/ws/events`);
    socket.addEventListener("open", () => socket.send(JSON.stringify({ type: "subscribe", projectId })));
    socket.addEventListener("message", () => { void loadAssets(); void loadReview(selectedId); });
    return () => socket.close();
  }, [loadAssets, loadReview, projectId, selectedId]);

  useEffect(() => {
    if (!selectedId) return;
    const timer = window.setInterval(() => {
      void loadAssets();
      void loadReview(selectedId);
    }, PROJECT_REFRESH_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [loadAssets, loadReview, selectedId]);

  const selectedIndex = assets.findIndex((asset) => asset.projectAssetId === selectedId);
  const move = useCallback((delta: number) => {
    if (!assets.length) return;
    const next = Math.min(assets.length - 1, Math.max(0, selectedIndex + delta));
    setSelectedId(assets[next]?.projectAssetId ?? null);
  }, [assets, selectedIndex]);

  const mutate = useCallback(async (
    optimistic: (asset: ProjectReviewAsset) => ProjectReviewAsset,
    command: (baseRevision: number) => Promise<ReviewMutationResult>,
  ) => {
    if (!review || syncState === "saving") return;
    const before = review;
    const optimisticAsset = optimistic(before);
    setReview(optimisticAsset);
    setAssets((current) => current.map((asset) => asset.projectAssetId === before.projectAssetId ? optimisticAsset : asset));
    setSyncState("saving");
    setError(null);
    try {
      const result = await command(before.materializedRevision);
      setReview((current) => current?.projectAssetId === result.asset.projectAssetId ? result.asset : current);
      setAssets((current) => current.map((asset) => asset.projectAssetId === result.asset.projectAssetId ? result.asset : asset));
      if (selectedId === result.asset.projectAssetId) setNoteDraft(result.asset.activeNote ?? "");
      setStaleNote(result.staleBaseRevision);
      setSyncState(result.asset.unresolvedConflictCount ? "conflict" : "synced");
    } catch (reason) {
      setReview((current) => current?.projectAssetId === before.projectAssetId ? before : current);
      setAssets((current) => current.map((asset) => asset.projectAssetId === before.projectAssetId ? before : asset));
      setSyncState(before.unresolvedConflictCount ? "conflict" : "synced");
      setError(normalizeError(reason));
    }
  }, [review, selectedId, syncState]);

  const toggleRemoved = useCallback(() => {
    if (!review || syncState === "saving") return;
    void mutate(
      (asset) => ({ ...asset, removed: !asset.removed }),
      (base) => client.setReviewRemoved(projectId, review.projectAssetId, !review.removed, base),
    );
  }, [client, mutate, projectId, review, syncState]);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const editing = target?.matches("input, textarea, select, [contenteditable='true']");
      if (event.key === "Escape" && editing) {
        event.preventDefault();
        target?.blur();
        setNoteDraft(review?.activeNote ?? "");
        return;
      }
      if (editing || event.metaKey || event.ctrlKey || event.altKey) return;
      if (event.key === "j" || event.key === "J" || event.key === "ArrowRight") { event.preventDefault(); move(1); }
      else if (event.key === "k" || event.key === "K" || event.key === "ArrowLeft") { event.preventDefault(); move(-1); }
      else if (event.key === " ") { event.preventDefault(); const video = videoRef.current; if (video) void (video.paused ? video.play() : Promise.resolve(video.pause())); }
      else if (event.key === "l" || event.key === "L") { event.preventDefault(); labelRef.current?.focus(); }
      else if (event.key === "n" || event.key === "N") { event.preventDefault(); noteRef.current?.focus(); }
      else if (event.key === "r" || event.key === "R") { event.preventDefault(); toggleRemoved(); }
      else if (event.key === "/") { event.preventDefault(); searchRef.current?.focus(); }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [move, review?.activeNote, toggleRemoved]);

  async function diagnosePlaybackFailure() {
    try {
      const session = await client.getSession();
      setPlaybackError(session.authenticated ? "media" : "session");
    } catch { setPlaybackError("media"); }
  }

  function saveNote(event?: FormEvent) {
    event?.preventDefault();
    if (!review) return;
    void mutate(
      (asset) => ({ ...asset, activeNote: noteDraft || null }),
      (base) => client.createReviewNote(projectId, review.projectAssetId, noteDraft, base),
    );
  }

  function addLabel(event: FormEvent) {
    event.preventDefault();
    const label = labelDraft.trim();
    if (!review || !label || review.labels.includes(label)) return;
    setLabelDraft("");
    void mutate(
      (asset) => ({ ...asset, labels: [...asset.labels, label].sort() }),
      (base) => client.addReviewLabel(projectId, review.projectAssetId, label, base),
    );
  }

  const proxyReady = review?.proxy?.state === "available" && review.proxy.availableOnServer;
  const poster = review?.thumbnail?.state === "available" && review.thumbnail.availableOnServer
    ? `/api/v1/media/${encodeURIComponent(review.thumbnail.representationId)}/content`
    : undefined;

  return <section className="workspace-page review-page">
    <div className="page-heading review-heading"><div><p className="eyebrow">Project / {projectId}</p><h1>Review workspace</h1></div><label className="project-search">Search this project<input ref={searchRef} value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Filename, camera, note, label" aria-keyshortcuts="/" /></label></div>
    {error ? <InlineError error={error} /> : null}
    {!assets.length ? <StatePanel title="No clips in this view"><p>Change the project search or add footage from the library.</p></StatePanel> : null}
    {review ? <div className="review-workbench">
      <aside className="review-queue" aria-label="Project clip queue">
        <div className="queue-counter"><strong>{selectedIndex + 1}</strong><span>/ {assets.length}</span></div>
        <div role="list">{assets.map((asset) => <button role="listitem" key={asset.projectAssetId} aria-current={asset.projectAssetId === selectedId} onClick={() => setSelectedId(asset.projectAssetId)}><span>{asset.filename}</span><small>{asset.camera || "Camera unknown"}</small></button>)}</div>
      </aside>
      <section className="review-stage-column" aria-label="Clip player">
        <div className="review-stage">
          {proxyReady ? <video key={review.proxy?.representationId} ref={videoRef} src={`/api/v1/media/${encodeURIComponent(review.proxy!.representationId)}/content`} poster={poster} preload="metadata" onError={() => void diagnosePlaybackFailure()} style={{ transform: `rotate(${review.rotationDegrees}deg)` }} /> : poster ? <img src={poster} alt="" /> : null}
          {!proxyReady ? <div className="representation-state"><p className="eyebrow">Proxy / {review.proxy?.state ?? "missing"}</p><h2>{proxyStateTitle(review)}</h2><p>{proxyStateDetail(review)}</p></div> : null}
          {playbackError === "session" ? <div className="stage-alert" role="alert"><span>Session expired. Sign in again before continuing playback.</span><button onClick={() => window.location.reload()}>Return to sign in</button></div> : playbackError === "media" ? <div className="stage-alert" role="alert">The proxy became unavailable. The original remains protected and is not served here.</div> : null}
        </div>
        <div className="transport"><button className="secondary" disabled={selectedIndex <= 0} onClick={() => move(-1)}>K · Previous</button><button onClick={() => { const video = videoRef.current; if (video) void (video.paused ? video.play() : Promise.resolve(video.pause())); }} disabled={!proxyReady}>Space · Play / pause</button><button className="secondary" disabled={selectedIndex >= assets.length - 1} onClick={() => move(1)}>J · Next</button></div>
        <div className="clip-strip"><span>{assets[selectedIndex - 1]?.filename ?? "Start of queue"}</span><strong>{review.filename}</strong><span>{assets[selectedIndex + 1]?.filename ?? "End of queue"}</span></div>
      </section>
      <aside className="review-inspector">
        <div className="inspector-status"><StatusBadge label={syncState === "saving" ? "Pending sync" : syncState === "conflict" ? "Conflict recorded" : "Synced"} tone={syncState === "conflict" ? "warning" : "positive"} /><span>rev {review.materializedRevision}</span></div>
        <h2>{review.filename}</h2><p className="clip-facts">{review.camera || "Camera unknown"} · {formatDuration(review.durationSeconds)} · {review.mediaVerificationState}</p>{proxyReady ? <p className="proxy-capability">Server proxy · browser playable · available for paired-Mac transfer</p> : null}
        {staleNote ? <p className="revision-notice">A newer note revision existed. Your revision was preserved and the canonical current note is shown.</p> : null}
        {review.unresolvedConflictCount ? <p className="revision-notice">{review.unresolvedConflictCount} unresolved operation {review.unresolvedConflictCount === 1 ? "conflict" : "conflicts"} recorded.</p> : null}
        <form onSubmit={saveNote} className="note-form"><label>Revision note<textarea ref={noteRef} value={noteDraft} onChange={(event) => setNoteDraft(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.metaKey && !event.ctrlKey && !event.shiftKey) { event.preventDefault(); saveNote(); } }} aria-keyshortcuts="N Enter Escape" /></label><button disabled={syncState === "saving" || noteDraft === (review.activeNote ?? "")}>Save note</button></form>
        <div className="label-section"><span className="field-label">Labels</span><div className="label-chips">{review.labels.map((label) => <button className="label-chip" key={label} disabled={syncState === "saving"} aria-label={`Remove label ${label}`} onClick={() => void mutate((asset) => ({ ...asset, labels: asset.labels.filter((value) => value !== label) }), (base) => client.removeReviewLabel(projectId, review.projectAssetId, label, base))}>{label}<b>×</b></button>)}</div><form onSubmit={addLabel}><input ref={labelRef} value={labelDraft} onChange={(event) => setLabelDraft(event.target.value)} placeholder="Add free-form label" aria-label="Add label" aria-keyshortcuts="L Enter" /><button disabled={syncState === "saving" || !labelDraft.trim()}>Add</button></form></div>
        <div className="inspector-actions"><button disabled={syncState === "saving"} className={review.removed ? "" : "secondary"} onClick={toggleRemoved}>{review.removed ? "Restore to queue" : "R · Remove from queue"}</button><button disabled={syncState === "saving"} className="secondary" onClick={() => { const next = (review.rotationDegrees + 90) % 360; void mutate((asset) => ({ ...asset, rotationDegrees: next }), (base) => client.setReviewRotation(projectId, review.projectAssetId, next, base)); }}>Rotate · {review.rotationDegrees}°</button></div>
        <p className="shortcut-map">J/K or ←/→ navigate · Space plays · L labels · N notes · R removes · / searches · Enter saves notes · Command+Enter inserts a newline · Escape leaves an editor</p>
      </aside>
    </div> : null}
  </section>;
}

function proxyStateTitle(asset: ProjectReviewAsset) {
  if (!asset.proxy) return "Proxy not generated";
  if (asset.proxy.state === "queued" || asset.proxy.state === "running") return "Proxy generation queued";
  if (asset.proxy.state === "available" && !asset.proxy.availableOnServer) return "Proxy is available on another Mac";
  if (asset.proxy.state === "failed") return "Proxy generation failed";
  return "Proxy unavailable";
}

function proxyStateDetail(asset: ProjectReviewAsset) {
  if (asset.proxy?.state === "available" && !asset.proxy.availableOnServer) return "This server does not have the review file yet. Use the paired Mac to transfer the compatible proxy, or regenerate it where the verified original is accessible.";
  if (asset.proxy?.state === "queued" || asset.proxy?.state === "running") return "The browser will use the canonical proxy when the server reports it available.";
  if (asset.mediaVerificationState === "mismatch") return "Identity must be repaired before a trustworthy proxy can be selected.";
  if (asset.originalAvailability === "accessible_here") return "The server can see the original, but browser review never streams originals. Queue a compatible proxy to enable playback.";
  if (asset.originalAvailability === "accessible_on_another_node") return "The original is available on a paired Mac only. Generate there or transfer a compatible proxy to this server.";
  if (asset.originalAvailability === "temporarily_unavailable" || asset.originalAvailability === "missing_after_completed_observation") return "The server original is offline. Reconnect its storage on the owning node, then generate or transfer a proxy.";
  return "Original availability is not registered here. Browser review requires a server-accessible compatible proxy.";
}

function formatDuration(seconds: number) {
  const rounded = Math.max(0, Math.round(seconds));
  return `${Math.floor(rounded / 60)}:${String(rounded % 60).padStart(2, "0")}`;
}

function AddToProjectDialog({ client, mediaObjectIds, close, complete }: { client: BrowserProjectClient; mediaObjectIds: string[]; close: () => void; complete: () => void }) {
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [projectId, setProjectId] = useState("");
  const [preview, setPreview] = useState<AddToProjectPreview | null>(null);
  const [acknowledge, setAcknowledge] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ProjectClientError | null>(null);
  const request = { mediaObjectIds, acknowledgeUnverified: acknowledge, idempotencyKey: `web_${mediaObjectIds.join("_")}`.slice(0, 250) };
  useEffect(() => { client.listProjects().then((page) => { setProjects(page.items); setProjectId(page.items[0]?.projectId ?? ""); }).catch((reason) => setError(normalizeError(reason))); }, [client]);
  useEffect(() => { if (projectId) client.previewAddToProject(projectId, request).then(setPreview).catch((reason) => setError(normalizeError(reason))); }, [client, projectId, mediaObjectIds.join(",")]);
  async function add() { setBusy(true); setError(null); try { const result = await client.addToProject(projectId, request); if (result.blockedCount === 0) complete(); else setPreview(await client.previewAddToProject(projectId, request)); } catch (reason) { setError(normalizeError(reason)); } finally { setBusy(false); } }
  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) close(); }}><section className="add-dialog" role="dialog" aria-modal="true" aria-labelledby="add-title"><p className="eyebrow">Replica command</p><h2 id="add-title">Add {mediaObjectIds.length} {mediaObjectIds.length === 1 ? "clip" : "clips"}</h2><label>Existing project<select value={projectId} onChange={(event) => setProjectId(event.target.value)}>{projects.map((project) => <option key={project.projectId}>{project.projectId}</option>)}</select></label>
    {preview ? <div className="preview-counts"><span><b>{preview.readyCount}</b> ready</span><span><b>{preview.alreadyMemberCount}</b> already there</span><span><b>{preview.acknowledgementRequiredCount}</b> provisional</span><span><b>{preview.mismatchCount}</b> blocked</span></div> : <p>Checking project membership…</p>}
    {preview?.acknowledgementRequiredCount ? <label className="acknowledge"><input type="checkbox" checked={acknowledge} onChange={(event) => setAcknowledge(event.target.checked)} /> Add provisional media with identity state recorded</label> : null}
    {error ? <InlineError error={error} /> : null}<div className="dialog-actions"><button className="secondary" onClick={close}>Cancel</button><button disabled={busy || !projectId || !!preview?.mismatchCount || (!!preview?.acknowledgementRequiredCount && !acknowledge)} onClick={() => void add()}>{busy ? "Adding…" : "Add to project"}</button></div></section></div>;
}

function availabilityLabel(value: string) { return value.replaceAll("_", " "); }
function formatBytes(value: number | null) { if (value == null) return "Size unknown"; const units = ["B", "KB", "MB", "GB", "TB"]; let amount = value; let unit = 0; while (amount >= 1000 && unit < units.length - 1) { amount /= 1000; unit += 1; } return `${amount.toFixed(unit ? 1 : 0)} ${units[unit]}`; }
function formatEta(seconds: number) { const rounded = Math.max(0, Math.round(seconds)); if (rounded < 60) return `${rounded}s`; const minutes = Math.floor(rounded / 60); if (minutes < 60) return `${minutes}m ${rounded % 60}s`; return `${Math.floor(minutes / 60)}h ${minutes % 60}m`; }

function StatusSummary({ status, compact = false }: { status: ServerStatus; compact?: boolean }) {
  const ready = status.readiness.status === "ready";
  return (
    <div className={compact ? "global-status compact" : "global-status"} aria-label="Server status">
      <StatusBadge label={ready ? "Server ready" : "Server needs attention"} tone={ready ? "positive" : "warning"} />
      {!compact && <span>{status.version.applicationVersion} · {status.version.nodeId}</span>}
    </div>
  );
}

function FilmRail({ compact = false }: { compact?: boolean }) {
  return <div className={compact ? "film-rail compact" : "film-rail"} aria-hidden="true">{Array.from({ length: 12 }, (_, index) => <i key={index} />)}</div>;
}

function StateScreen({ title, detail, busy = false }: { title: string; detail: string; busy?: boolean }) {
  return (
    <main className="state-layout">
      <StatePanel title={title} busy={busy}><p>{detail}</p></StatePanel>
    </main>
  );
}

function ErrorScreen({ error, retry }: { error: ProjectClientError; retry: () => void }) {
  const copy = {
    server_unavailable: ["Server unavailable", "Check that the server is running and this device is connected to the private LAN or tailnet."],
    session_expired: ["Session expired", "Sign in again with a new bootstrap code."],
    credential_revoked: ["Credential revoked", "An administrator revoked this browser credential."],
    csrf_failure: ["Security token expired", "Sign in again to refresh this browser session."],
    protocol_mismatch: ["Update required", error.message],
    server_error: ["Server request failed", "Retry the request. Use the request ID when checking server logs."],
  }[error.kind];
  return (
    <main className="state-layout">
      <StatePanel title={copy[0]} action={<button onClick={retry}>Try again</button>}>
        <p>{copy[1]}</p>
        {error.requestId && <p className="request-id">Request ID: {error.requestId}</p>}
      </StatePanel>
    </main>
  );
}

function InlineError({ error }: { error: ProjectClientError }) {
  return <p className="inline-error" role="alert">{error.message}{error.requestId ? ` Request ID: ${error.requestId}` : ""}</p>;
}

function normalizeError(error: unknown): ProjectClientError {
  return error instanceof ProjectClientError
    ? error
    : new ProjectClientError("server_error", "The application could not complete the request.", "unexpected_error");
}
