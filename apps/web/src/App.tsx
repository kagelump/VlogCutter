import { useEffect, useMemo, useState, type FormEvent } from "react";
import {
  ProjectClientError,
  type AddToProjectPreview,
  type BrowserProjectClient,
  type LibrarySearchItem,
  type LibrarySort,
  type ProjectAssetItem,
  type ProjectSummary,
  type ServerStatus,
  type SessionState,
} from "@rcdvlog/project-client";
import { StatePanel, StatusBadge } from "@rcdvlog/review-ui";

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
        ) : <Overview status={status} session={session} />}
      </main>
    </div>
  );
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
  const [error, setError] = useState<ProjectClientError | null>(null);
  useEffect(() => { client.listProjectAssets(projectId, 100, undefined, search || undefined).then((page) => setAssets(page.items)).catch((reason) => setError(normalizeError(reason))); }, [client, projectId, search]);
  return <section className="workspace-page"><div className="page-heading"><div><p className="eyebrow">Project / {projectId}</p><h1>Project assets</h1></div><label className="project-search">Search this project<input value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Filename, camera, note, label" /></label></div>
    {error ? <InlineError error={error} /> : null}<div className="asset-table" role="list">{assets.map((asset) => <article role="listitem" key={asset.projectAssetId}><div><strong>{asset.filename}</strong><span>{asset.camera || "Camera unknown"}</span></div><span>{asset.labels.join(", ") || "No labels"}</span><span>{asset.activeNote || "No note"}</span><StatusBadge label={asset.mediaVerificationState} tone={asset.mediaVerificationState === "verified" ? "positive" : "warning"} /></article>)}</div>
  </section>;
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
