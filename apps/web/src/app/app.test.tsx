import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ProjectClientError } from "@rcdvlog/project-client";
import { readyStatus, renderApp } from "../test/render";
import { legacyTvReviewPath } from "./paths";

const project = { projectId: "project_f298ae3a921d", state: "registered", assetCount: 1468, registeredAt: "2026-11-02T00:00:00Z", updatedAt: "2026-11-02T00:00:00Z" };

test("signed-out visitors get the sign-in form, and a valid code opens the app", async () => {
  const getSession = vi.fn()
    .mockResolvedValueOnce({ authenticated: false, sessions: [] })
    .mockResolvedValue({ authenticated: true, sessions: [] });
  const { client } = renderApp("/", {
    getSession,
    loginWithBootstrapCode: vi.fn().mockResolvedValue({ requestId: "r", sessionId: "s", displayPrefix: "1234", expiresAt: "t" }),
    listProjects: vi.fn().mockResolvedValue({ items: [project], nextCursor: null }),
  });
  expect(await screen.findByRole("heading", { name: "Sign in" })).toBeVisible();
  expect(screen.getByRole("button", { name: "Sign in" })).toBeDisabled();
  await userEvent.type(screen.getByLabelText("Sign-in code"), "  ABCD-1234 ");
  await userEvent.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("heading", { name: "Projects" })).toBeVisible();
  expect(client.loginWithBootstrapCode).toHaveBeenCalledWith("ABCD-1234");
  expect(screen.getByRole("heading", { name: "Project f298ae3a" })).toBeVisible();
  expect(screen.getByText(/1,468 clips/)).toBeVisible();
});

test("a rejected code explains itself and keeps the form", async () => {
  renderApp("/", {
    getSession: vi.fn().mockResolvedValue({ authenticated: false, sessions: [] }),
    loginWithBootstrapCode: vi.fn().mockRejectedValue(new ProjectClientError("server_error", "Bootstrap code is invalid or expired.", "invalid_bootstrap_code", "request_1", 401)),
  });
  await userEvent.type(await screen.findByLabelText("Sign-in code"), "nope");
  await userEvent.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Bootstrap code is invalid or expired.");
  expect(screen.getByRole("alert")).toHaveTextContent("Request request_1");
});

test("an unreachable server offers a retry instead of a blank page", async () => {
  const getServerStatus = vi.fn()
    .mockRejectedValueOnce(new ProjectClientError("server_unavailable", "down", "server_unavailable"))
    .mockResolvedValue(readyStatus);
  const getSession = vi.fn()
    .mockRejectedValueOnce(new ProjectClientError("server_unavailable", "down", "server_unavailable"))
    .mockResolvedValue({ authenticated: true, sessions: [] });
  renderApp("/", { getServerStatus, getSession, listProjects: vi.fn().mockResolvedValue({ items: [], nextCursor: null }) });
  expect(await screen.findByRole("heading", { name: "Can’t open VlogCutter" })).toBeVisible();
  expect(screen.getByRole("alert")).toHaveTextContent("Can't reach the server");
  await userEvent.click(screen.getByRole("button", { name: "Try again" }));
  expect(await screen.findByText("No projects yet")).toBeVisible();
});

test("a server speaking a different protocol blocks the app", async () => {
  renderApp("/", {
    getServerStatus: vi.fn().mockRejectedValue(new ProjectClientError("protocol_mismatch", "This web app supports protocol 1, but the server uses 2.", "protocol_mismatch")),
  });
  expect(await screen.findByRole("heading", { name: "Can’t open VlogCutter" })).toBeVisible();
  expect(screen.getByRole("alert")).toHaveTextContent("out of date");
});

test("failing server checks are named in the header", async () => {
  renderApp("/", {
    getServerStatus: vi.fn().mockResolvedValue({
      ...readyStatus,
      readiness: { requestId: "r", status: "not_ready", checks: [{ name: "tool_exiftool", status: "error", code: "tool_missing" }] },
    }),
    listProjects: vi.fn().mockResolvedValue({ items: [], nextCursor: null }),
  });
  expect(await screen.findByRole("link", { name: /exiftool not found/ })).toHaveAttribute("href", "/admin");
});

test("old /tv-review addresses land on the same page in the new layout", async () => {
  expect(legacyTvReviewPath("p")).toBe("/projects/p/reviews");
  expect(legacyTvReviewPath("p/t")).toBe("/projects/p/reviews/t");
  expect(legacyTvReviewPath("p/t/record")).toBe("/projects/p/reviews/t/record");
  expect(legacyTvReviewPath("p/t/sessions/s")).toBe("/projects/p/reviews/t/sessions/s");
  expect(legacyTvReviewPath("p/t/sessions/s/review/r")).toBe("/projects/p/reviews/t/sessions/s/runs/r");
  const { router } = renderApp("/tv-review/project_x", { listReviewTargets: vi.fn().mockResolvedValue({ items: [], nextBefore: null }) });
  await waitFor(() => expect(router.state.location.pathname).toBe("/projects/project_x/reviews"));
  expect(await screen.findByText("No cuts to review yet")).toBeVisible();
});

test("unknown addresses show a not-found page inside the app", async () => {
  renderApp("/nowhere");
  expect(await screen.findByRole("heading", { name: "Page not found" })).toBeVisible();
  expect(screen.getByRole("navigation", { name: "Main" })).toBeVisible();
});
