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
    getAdminOverview: vi.fn(),
    listAdminAudit: vi.fn(),
    listAdminStorageRoots: vi.fn(),
    createAdminStorageRoot: vi.fn(),
    updateAdminStorageRoot: vi.fn(),
    listAdminStorageBindings: vi.fn(),
    upsertAdminStorageBinding: vi.fn(),
    verifyAdminStorageBinding: vi.fn(),
    confirmAdminStorageBinding: vi.fn(),
    listAdminScans: vi.fn(),
    listAdminScanDiagnostics: vi.fn(),
    requestAdminScan: vi.fn(),
    listAdminScanSchedules: vi.fn(),
    createAdminScanSchedule: vi.fn(),
    updateAdminScanSchedule: vi.fn(),
    listAdminJobs: vi.fn(),
    getAdminJob: vi.fn(),
    cancelAdminJob: vi.fn(),
    retryAdminJob: vi.fn(),
    listAdminSync: vi.fn(),
    requestAdminRepresentationBatch: vi.fn(),
    listDevices: vi.fn(),
    revokeDevice: vi.fn(),
    revokeSession: vi.fn(),
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

test("administration loads durable projections and confirms root registration", async () => {
  window.history.replaceState({}, "", "/admin");
  const mock = client({ authenticated: true, sessions: [] });
  vi.mocked(mock.getAdminOverview).mockResolvedValue({
    applicationVersion: "0.1.0", protocolMajor: 1, protocolMinor: 0, controlSchemaVersion: 9, nodeId: "node_fixture",
    readiness: { status: "ready", checks: [{ name: "control_catalog", status: "ok" }] },
    config: { dataDir: "/state", tempDir: "/state/tmp", derivedDir: "/derived", backupDir: "/backup", allowedOriginalMounts: ["/archive"], listenAddr: "127.0.0.1:8090", shutdownTimeoutSeconds: 30, trustedProxyCidrCount: 0, lanHostname: "localhost", sessionMaxAgeSeconds: 3600, bootstrapCodeTtlSeconds: 300, pairingCodeTtlSeconds: 300, backupIntervalSeconds: 86400, backupDailyRetention: 14, backupWeeklyRetention: 8, minimumFreeBytes: 64 * 1024 * 1024, ffmpegCommand: "ffmpeg", ffprobeCommand: "ffprobe", exiftoolCommand: "exiftool" },
    auth: { activeSessions: 1, activeDeviceCredentials: 1, pendingBootstrapCodes: 0, nodeId: "node_fixture" },
    derivedAvailableBytes: 1_000_000, backupAvailableBytes: 2_000_000,
  });
  vi.mocked(mock.listAdminStorageRoots).mockResolvedValue({ items: [] });
  vi.mocked(mock.listAdminStorageBindings).mockResolvedValue({ items: [] });
  vi.mocked(mock.listAdminScans).mockResolvedValue({ items: [] });
  vi.mocked(mock.listAdminScanSchedules).mockResolvedValue({ items: [] });
  vi.mocked(mock.listAdminJobs).mockResolvedValue({ items: [], nextBefore: null });
  vi.mocked(mock.listAdminSync).mockResolvedValue({ items: [] });
  vi.mocked(mock.listDevices).mockResolvedValue([]);
  vi.mocked(mock.listAdminAudit).mockResolvedValue({ items: [] });
  vi.mocked(mock.createAdminStorageRoot).mockResolvedValue({ id: "root_1", displayName: "Archive", immutableOriginal: true, enabled: true, disabledAt: null, createdAt: "now", updatedAt: "now" });
  render(<App client={mock} />);
  expect(await screen.findByRole("heading", { name: "Administration" })).toBeVisible();
  expect(await screen.findByText("All prerequisites ready")).toBeVisible();
  await userEvent.type(screen.getByLabelText("Logical root name"), "Archive");
  await userEvent.click(screen.getByRole("button", { name: "Register root" }));
  await waitFor(() => expect(mock.createAdminStorageRoot).toHaveBeenCalledWith("Archive"));
  window.history.replaceState({}, "", "/");
});
