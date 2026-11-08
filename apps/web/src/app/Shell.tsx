import type { ReactNode } from "react";
import { Link, Outlet, ScrollRestoration, isRouteErrorResponse, useLocation, useRouteError } from "react-router";
import { useQuery } from "@tanstack/react-query";
import type { ServerStatus } from "@rcdvlog/project-client";
import { LoginPage } from "../features/auth/LoginPage";
import { describeCheck, toClientError } from "../lib/errors";
import { Button, EmptyState, ErrorMessage, Icon, Loading, type IconName } from "../ui";
import { keys, useClient } from "./client";
import styles from "./Shell.module.css";

const NAV: Array<{ to: string; label: string; icon: IconName; matches: (path: string) => boolean }> = [
  { to: "/", label: "Projects", icon: "folder", matches: (path) => path === "/" || path.startsWith("/projects") },
  { to: "/library", label: "Library", icon: "film", matches: (path) => path.startsWith("/library") },
  { to: "/admin", label: "Admin", icon: "settings", matches: (path) => path.startsWith("/admin") },
];

/** Root route: resolves the server and session, then shows sign-in or the app. */
export function Root() {
  const client = useClient();
  const status = useQuery({ queryKey: keys.status, queryFn: () => client.getServerStatus(), refetchInterval: 60_000 });
  const { pathname } = useLocation();
  const session = useQuery({ queryKey: keys.auth, queryFn: () => client.getSession(), staleTime: 60_000 });

  if (session.isPending || (status.isPending && !status.error)) return <FullScreen><Loading label="Connecting…" center /></FullScreen>;
  // Status is informational, except that an incompatible server must not be driven by this page.
  const incompatible = status.error && toClientError(status.error).kind === "protocol_mismatch" ? status.error : null;
  const error = incompatible ?? (session.data ? null : session.error);
  if (error) {
    return (
      <FullScreen>
        <div className={styles.fatal}>
          <h1>Can’t open VlogCutter</h1>
          <ErrorMessage error={error} />
          <Button variant="primary" onClick={() => { void status.refetch(); void session.refetch(); }}>Try again</Button>
        </div>
      </FullScreen>
    );
  }
  if (!session.data?.authenticated) return <LoginPage />;

  return (
    <div className={styles.shell}>
      <header className={styles.header}>
        <Link to="/" className={styles.brand} aria-label="VlogCutter home">
          <span className={styles.logo} aria-hidden="true" />
          <span className={styles.brandText}>VlogCutter</span>
        </Link>
        <nav className={styles.nav} aria-label="Main">
          {NAV.map((item) => (
            <Link key={item.to} to={item.to} className={styles.navLink} aria-current={item.matches(pathname) ? "page" : undefined}>
              <Icon name={item.icon} size={17} />
              <span>{item.label}</span>
            </Link>
          ))}
        </nav>
        {status.data ? <ServerHealth status={status.data} /> : null}
      </header>
      <main className={styles.main}>
        <Outlet />
      </main>
      <ScrollRestoration />
    </div>
  );
}

function ServerHealth({ status }: { status: ServerStatus }) {
  const failing = status.readiness.checks.filter((check) => check.status !== "ok");
  if (status.readiness.status === "ready" && !failing.length) return null;
  const summary = failing.map(describeCheck).join("\n");
  return (
    <Link to="/admin" className={styles.health} title={summary || "Server needs attention"}>
      <span className={styles.healthDot} aria-hidden="true" />
      <span className={styles.healthLabel}>{failing.length === 1 ? describeCheck(failing[0]) : `${failing.length} server issues`}</span>
      <span className="visually-hidden">: {summary}</span>
    </Link>
  );
}

function FullScreen({ children }: { children: ReactNode }) {
  return <main className={styles.fullScreen}>{children}</main>;
}

/** A page body. `wide` suits grids; `full` pages manage their own width (players, workspaces). */
export function Page({ children, full = false, wide = false }: { children: ReactNode; full?: boolean; wide?: boolean }) {
  if (full) return <div className={styles.pageFull} data-full-page="">{children}</div>;
  return <div className={wide ? `${styles.page} ${styles.wide}` : styles.page}>{children}</div>;
}

export function RouteError() {
  const error = useRouteError();
  if (isRouteErrorResponse(error) && error.status === 404) return <NotFound />;
  console.error("route_render_failed", error);
  return (
    <FullScreen>
      <div className={styles.fatal}>
        <h1>Something broke</h1>
        <p>Reload the page. If it keeps happening, check the browser console and server logs.</p>
        <Button variant="primary" onClick={() => window.location.reload()}>Reload</Button>
      </div>
    </FullScreen>
  );
}

export function NotFound() {
  return (
    <Page>
      <EmptyState title="Page not found" action={<Link to="/">Go to projects</Link>}>
        This address doesn’t match anything in VlogCutter.
      </EmptyState>
    </Page>
  );
}
