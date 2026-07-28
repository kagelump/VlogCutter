import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { BrowserProjectClient, ServerStatus, SessionState } from "@rcdvlog/project-client";
import App from "./App";

const status: ServerStatus = {
  version: {
    requestId: "request_fixture",
    applicationVersion: "0.1.0",
    protocolVersion: { major: 1, minor: 0 },
    controlSchemaVersion: 8,
    nodeId: "node_fixture",
  },
  readiness: { requestId: "request_ready", status: "ready", checks: [] },
};

function client(session: SessionState): BrowserProjectClient {
  return {
    getServerStatus: vi.fn().mockResolvedValue(status),
    getSession: vi.fn().mockResolvedValue(session),
    loginWithBootstrapCode: vi.fn().mockResolvedValue({
      requestId: "request_login",
      sessionId: "session_fixture",
      displayPrefix: "12345678",
      expiresAt: "2026-07-25T00:00:00Z",
    }),
    listLibraryMedia: vi.fn().mockResolvedValue({ items: [], nextCursor: null, availabilityFilterApplied: false }),
    listProjects: vi.fn().mockResolvedValue({ items: [], nextCursor: null }),
    listProjectAssets: vi.fn().mockResolvedValue({ items: [], nextCursor: null }),
    previewAddToProject: vi.fn(),
    addToProject: vi.fn(),
  };
}

test("shows the login flow before product data", async () => {
  render(<App client={client({ authenticated: false, sessions: [] })} />);
  expect(await screen.findByRole("heading", { name: /Your footage stays close/ })).toBeVisible();
  expect(screen.getByLabelText("Bootstrap code")).toBeVisible();
});

test("loads the authenticated application shell", async () => {
  render(<App client={client({ authenticated: true, sessions: [] })} />);
  expect(await screen.findByRole("heading", { name: "Review desk ready" })).toBeVisible();
  expect(screen.getByText("Server ready")).toBeVisible();
});

test("submits a bootstrap code through the client seam", async () => {
  const mock = client({ authenticated: false, sessions: [] });
  render(<App client={mock} />);
  const input = await screen.findByLabelText("Bootstrap code");
  await userEvent.type(input, "ABC123");
  await userEvent.click(screen.getByRole("button", { name: "Open review desk" }));
  await waitFor(() => expect(mock.loginWithBootstrapCode).toHaveBeenCalledWith("ABC123"));
});

test("library route uses bounded query state from browser history", async () => {
  window.history.replaceState({}, "", "/library?q=C01&sort=filenameAsc");
  const mock = client({ authenticated: true, sessions: [] });
  render(<App client={mock} />);
  expect(await screen.findByRole("heading", { name: "Library" })).toBeVisible();
  await waitFor(() => expect(mock.listLibraryMedia).toHaveBeenCalledWith(expect.objectContaining({
    limit: 48,
    filenamePrefix: "C01",
    sort: "filenameAsc",
  })));
  window.history.replaceState({}, "", "/");
});
