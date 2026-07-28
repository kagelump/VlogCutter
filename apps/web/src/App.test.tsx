import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ProjectClientError, type BrowserProjectClient, type ProjectReviewAsset, type ServerStatus, type SessionState } from "@rcdvlog/project-client";
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

const reviewAsset: ProjectReviewAsset = {
  projectAssetId: "asset_fixture",
  filename: "C0103.MOV",
  camera: "Sony A7S III",
  mediaObjectId: "media_fixture",
  mediaVerificationState: "verified",
  removed: false,
  rotationDegrees: 0,
  labels: ["arrival"],
  activeNote: "First look",
  durationSeconds: 65,
  bestTime: "2026-07-25T00:00:00Z",
  materializedRevision: 4,
  activeNoteRevisionId: "note_1",
  noteRevisionCount: 1,
  proxy: null,
  thumbnail: null,
  originalAvailability: "accessible_on_another_node",
  unresolvedConflictCount: 0,
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
    getProjectReviewAsset: vi.fn(),
    createReviewNote: vi.fn(),
    addReviewLabel: vi.fn(),
    removeReviewLabel: vi.fn(),
    setReviewRemoved: vi.fn(),
    setReviewRotation: vi.fn(),
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

test("project review exposes explicit proxy state and keyboard guidance", async () => {
  window.history.replaceState({}, "", "/projects/project_fixture");
  const mock = client({ authenticated: true, sessions: [] });
  vi.mocked(mock.listProjectAssets).mockResolvedValue({ items: [reviewAsset], nextCursor: null });
  vi.mocked(mock.getProjectReviewAsset).mockResolvedValue(reviewAsset);
  render(<App client={mock} />);
  expect(await screen.findByRole("heading", { name: "Proxy not generated" })).toBeVisible();
  expect(screen.getByText(/paired Mac only/)).toBeVisible();
  expect(screen.getByText(/J\/K or/)).toBeVisible();
  window.history.replaceState({}, "", "/");
});

test("stale note writes remain committed and surface revision context", async () => {
  window.history.replaceState({}, "", "/projects/project_fixture");
  const mock = client({ authenticated: true, sessions: [] });
  vi.mocked(mock.listProjectAssets).mockResolvedValue({ items: [reviewAsset], nextCursor: null });
  vi.mocked(mock.getProjectReviewAsset).mockResolvedValue(reviewAsset);
  vi.mocked(mock.createReviewNote).mockResolvedValue({
    asset: { ...reviewAsset, activeNote: "Browser revision", materializedRevision: 6, noteRevisionCount: 3 },
    staleBaseRevision: true,
  });
  render(<App client={mock} />);
  const note = await screen.findByRole("textbox", { name: "Revision note" });
  await userEvent.clear(note);
  await userEvent.type(note, "Browser revision");
  await userEvent.click(screen.getByRole("button", { name: "Save note" }));
  await waitFor(() => expect(mock.createReviewNote).toHaveBeenCalledWith("project_fixture", "asset_fixture", "Browser revision", 4));
  expect(await screen.findByText(/newer note revision existed/)).toBeVisible();
  window.history.replaceState({}, "", "/");
});

test("failed optimistic removal restores the canonical clip and reports the request id", async () => {
  window.history.replaceState({}, "", "/projects/project_fixture");
  const mock = client({ authenticated: true, sessions: [] });
  vi.mocked(mock.listProjectAssets).mockResolvedValue({ items: [reviewAsset], nextCursor: null });
  vi.mocked(mock.getProjectReviewAsset).mockResolvedValue(reviewAsset);
  vi.mocked(mock.setReviewRemoved).mockRejectedValue(new ProjectClientError("server_error", "Mutation failed.", "write_failed", "request_rollback"));
  render(<App client={mock} />);
  await userEvent.click(await screen.findByRole("button", { name: "R · Remove from queue" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("request_rollback");
  expect(screen.getByRole("button", { name: "R · Remove from queue" })).toBeVisible();
  window.history.replaceState({}, "", "/");
});
