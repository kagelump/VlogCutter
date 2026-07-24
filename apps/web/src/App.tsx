import { useEffect, useState, type FormEvent } from "react";
import {
  ProjectClientError,
  type BrowserProjectClient,
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
  return <AuthenticatedShell status={state.status} session={state.session} />;
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

function AuthenticatedShell({ status, session }: { status: ServerStatus; session: SessionState }) {
  const [path, setPath] = useState(window.location.pathname);
  const routes = [
    { path: "/", label: "Overview" },
    { path: "/library", label: "Library" },
    { path: "/projects", label: "Projects" },
  ];

  function navigate(next: string) {
    window.history.pushState({}, "", next);
    setPath(next);
  }

  useEffect(() => {
    const pop = () => setPath(window.location.pathname);
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
        <p className="eyebrow">Server workspace</p>
        <h1>{path === "/" ? "Review desk ready" : path === "/library" ? "Library" : "Projects"}</h1>
        <p className="lede">
          {path === "/"
            ? "The secure web foundation is connected. Library, project, and review tools arrive in the next feature slices."
            : "This route is ready for the next server feature slice."}
        </p>
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
      </main>
    </div>
  );
}

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
