import {
  assertProtocolCompatible,
  ProjectClientError,
  type BrowserProjectClient,
  type AddToProjectPreview,
  type AddToProjectRequest,
  type AddToProjectResult,
  type AdminAuditEvent,
  type AdminJob,
  type AdminJobPage,
  type AdminJobState,
  type AdminOverview,
  type DeviceCredentialInfo,
  type LibraryQueryParams,
  type LibrarySearchPage,
  type LoginResult,
  type ProjectAssetPage,
  type ProjectReviewAsset,
  type ProjectPage,
  type ProjectSyncOverview,
  type RepresentationBatchResult,
  type ReviewMutationResult,
  type ServerReadiness,
  type ServerStatus,
  type ServerVersion,
  type SessionState,
  type ScanRun,
  type ScanFileDiagnostic,
  type ScanSchedule,
  type StorageBinding,
  type StorageRoot,
} from "./index";
import { API_PATHS } from "./generated";

export interface TokenStore {
  get(): string | null;
  set(value: string): void;
  clear(): void;
}

const memoryTokenStore = (): TokenStore => {
  let value: string | null = null;
  return {
    get: () => value,
    set: (next) => { value = next; },
    clear: () => { value = null; },
  };
};

interface ErrorBody {
  code?: string;
  requestId?: string;
}

export class HttpProjectClient implements BrowserProjectClient {
  private readonly baseUrl: string;

  constructor(
    baseUrl = "",
    private readonly fetcher: typeof fetch = fetch,
    private readonly tokens: TokenStore = memoryTokenStore(),
  ) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
  }

  async getServerStatus(): Promise<ServerStatus> {
    try {
      const [version, readiness] = await Promise.all([
        this.request<ServerVersion>(API_PATHS.version),
        this.request<ServerReadiness>(API_PATHS.ready, undefined, true),
      ]);
      assertProtocolCompatible(version);
      return { version, readiness };
    } catch (error) {
      if (error instanceof ProjectClientError) throw error;
      throw unavailableError(error);
    }
  }

  async getSession(): Promise<SessionState> {
    try {
      const response = await this.request<{ sessions: SessionState["sessions"] }>(
        API_PATHS.sessions,
      );
      return { authenticated: true, sessions: response.sessions };
    } catch (error) {
      if (error instanceof ProjectClientError && error.kind === "session_expired") {
        this.tokens.clear();
        return { authenticated: false, sessions: [] };
      }
      throw error;
    }
  }

  async loginWithBootstrapCode(code: string, hostname = globalThis.location?.hostname ?? "chrome"): Promise<LoginResult> {
    const response = await this.fetcher.call(globalThis, `${this.baseUrl}${API_PATHS.bootstrapExchange}`, {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/json", "x-request-id": requestId() },
      body: JSON.stringify({ code, hostname }),
    }).catch((error: unknown) => { throw unavailableError(error); });
    if (!response.ok) throw await responseError(response);
    const csrf = response.headers.get("x-csrf-token");
    if (!csrf) {
      throw new ProjectClientError(
        "server_error",
        "The server created a session without a CSRF token.",
        "missing_csrf_token",
        response.headers.get("x-request-id"),
        response.status,
      );
    }
    this.tokens.set(csrf);
    return response.json() as Promise<LoginResult>;
  }

  async listLibraryMedia(query: LibraryQueryParams = {}): Promise<LibrarySearchPage> {
    return this.request<LibrarySearchPage>(`${API_PATHS.libraryMedia}${queryString(query)}`);
  }

  async listProjects(limit = 50, after?: string): Promise<ProjectPage> {
    return this.request<ProjectPage>(`${API_PATHS.projects}${queryString({ limit, after })}`);
  }

  async listProjectAssets(projectId: string, limit = 50, after?: string, search?: string): Promise<ProjectAssetPage> {
    const path = API_PATHS.projectAssets.replace("{projectId}", encodeURIComponent(projectId));
    return this.request<ProjectAssetPage>(`${path}${queryString({ limit, after, search })}`);
  }

  async getProjectReviewAsset(projectId: string, assetId: string): Promise<ProjectReviewAsset> {
    return this.request<ProjectReviewAsset>(reviewPath(API_PATHS.projectReviewAsset, projectId, assetId));
  }

  async createReviewNote(projectId: string, assetId: string, text: string, baseRevision: number): Promise<ReviewMutationResult> {
    return this.request<ReviewMutationResult>(reviewPath(API_PATHS.createReviewNote, projectId, assetId), jsonPost({ text, baseRevision }));
  }

  async addReviewLabel(projectId: string, assetId: string, label: string, baseRevision: number): Promise<ReviewMutationResult> {
    return this.request<ReviewMutationResult>(reviewPath(API_PATHS.addReviewLabel, projectId, assetId), jsonPost({ label, baseRevision }));
  }

  async removeReviewLabel(projectId: string, assetId: string, label: string, baseRevision: number): Promise<ReviewMutationResult> {
    return this.request<ReviewMutationResult>(reviewPath(API_PATHS.removeReviewLabel, projectId, assetId), jsonPost({ label, baseRevision }));
  }

  async setReviewRemoved(projectId: string, assetId: string, removed: boolean, baseRevision: number): Promise<ReviewMutationResult> {
    return this.request<ReviewMutationResult>(reviewPath(API_PATHS.setReviewRemoved, projectId, assetId), jsonPost({ removed, baseRevision }));
  }

  async setReviewRotation(projectId: string, assetId: string, rotationDegrees: number, baseRevision: number): Promise<ReviewMutationResult> {
    return this.request<ReviewMutationResult>(reviewPath(API_PATHS.setReviewRotation, projectId, assetId), jsonPost({ rotationDegrees, baseRevision }));
  }

  async previewAddToProject(projectId: string, request: AddToProjectRequest): Promise<AddToProjectPreview> {
    return this.request<AddToProjectPreview>(
      API_PATHS.previewAddToProject.replace("{projectId}", encodeURIComponent(projectId)),
      jsonPost(request),
    );
  }

  async addToProject(projectId: string, request: AddToProjectRequest): Promise<AddToProjectResult> {
    return this.request<AddToProjectResult>(
      API_PATHS.addToProject.replace("{projectId}", encodeURIComponent(projectId)),
      jsonPost(request),
    );
  }

  async getAdminOverview(): Promise<AdminOverview> { return this.request(API_PATHS.adminOverview); }
  async listAdminAudit(limit = 50, before?: number): Promise<{ items: AdminAuditEvent[] }> { return this.request(`${API_PATHS.adminAudit}${queryString({ limit, before })}`); }
  async listAdminStorageRoots(): Promise<{ items: StorageRoot[] }> { return this.request(API_PATHS.adminStorageRoots); }
  async createAdminStorageRoot(displayName: string, immutableOriginal = true): Promise<StorageRoot> { return this.request(API_PATHS.adminStorageRoots, jsonPost({ displayName, immutableOriginal })); }
  async updateAdminStorageRoot(rootId: string, update: { displayName?: string; enabled?: boolean; confirmDisable?: boolean }): Promise<StorageRoot> { return this.request(pathParam(API_PATHS.adminStorageRoot, "rootId", rootId), jsonRequest("PATCH", update)); }
  async listAdminStorageBindings(): Promise<{ items: StorageBinding[] }> { return this.request(API_PATHS.adminStorageBindings); }
  async upsertAdminStorageBinding(rootId: string, request: Parameters<BrowserProjectClient["upsertAdminStorageBinding"]>[1]): Promise<StorageBinding> { return this.request(pathParam(API_PATHS.adminStorageBinding, "rootId", rootId), jsonRequest("PUT", request)); }
  async verifyAdminStorageBinding(rootId: string): Promise<StorageBinding> { return this.request(pathParam(API_PATHS.adminStorageBindingVerification, "rootId", rootId), jsonPost({})); }
  async confirmAdminStorageBinding(rootId: string, action: "confirm" | "reject"): Promise<StorageBinding> { return this.request(pathParam(API_PATHS.adminStorageBindingConfirmation, "rootId", rootId), jsonPost({ action })); }
  async listAdminScans(limit = 50): Promise<{ items: ScanRun[] }> { return this.request(`${API_PATHS.adminScans}${queryString({ limit })}`); }
  async listAdminScanDiagnostics(scanId: string, limit = 50): Promise<{ items: ScanFileDiagnostic[] }> { return this.request(`${pathParam(API_PATHS.adminScanDiagnostics, "scanId", scanId)}${queryString({ limit })}`); }
  async requestAdminScan(request: Parameters<BrowserProjectClient["requestAdminScan"]>[0]): Promise<ScanRun> { return this.request(API_PATHS.adminScans, jsonPost(request)); }
  async listAdminScanSchedules(): Promise<{ items: ScanSchedule[] }> { return this.request(API_PATHS.adminScanSchedules); }
  async createAdminScanSchedule(request: Parameters<BrowserProjectClient["createAdminScanSchedule"]>[0]): Promise<ScanSchedule> { return this.request(API_PATHS.adminScanSchedules, jsonPost(request)); }
  async updateAdminScanSchedule(scheduleId: string, update: Parameters<BrowserProjectClient["updateAdminScanSchedule"]>[1]): Promise<ScanSchedule> { return this.request(pathParam(API_PATHS.adminScanSchedule, "scheduleId", scheduleId), jsonRequest("PATCH", update)); }
  async listAdminJobs(limit = 50, before?: number, state?: AdminJobState): Promise<AdminJobPage> { return this.request(`${API_PATHS.adminJobs}${queryString({ limit, before, state })}`); }
  async getAdminJob(jobId: string): Promise<AdminJob> { return this.request(pathParam(API_PATHS.adminJob, "jobId", jobId)); }
  async cancelAdminJob(jobId: string): Promise<AdminJob> { return this.request(pathParam(API_PATHS.adminJobCancel, "jobId", jobId), jsonPost({})); }
  async retryAdminJob(jobId: string): Promise<AdminJob> { return this.request(pathParam(API_PATHS.adminJobRetry, "jobId", jobId), jsonPost({})); }
  async listAdminSync(): Promise<{ items: ProjectSyncOverview[] }> { return this.request(API_PATHS.adminSync); }
  async requestAdminRepresentationBatch(request: Parameters<BrowserProjectClient["requestAdminRepresentationBatch"]>[0]): Promise<RepresentationBatchResult> { return this.request(API_PATHS.adminRepresentationBatch, jsonPost(request)); }
  async listDevices(): Promise<DeviceCredentialInfo[]> { const result = await this.request<{ devices: DeviceCredentialInfo[] }>(API_PATHS.devices); return result.devices; }
  async revokeDevice(credentialId: string): Promise<void> { await this.request(pathParam(API_PATHS.revokeDevice, "credentialId", credentialId), { method: "DELETE" }); }
  async revokeSession(sessionId: string): Promise<void> { await this.request(pathParam(API_PATHS.revokeSession, "sessionId", sessionId), { method: "DELETE" }); }

  private async request<T>(
    path: string,
    init?: RequestInit,
    allowServiceUnavailable = false,
  ): Promise<T> {
    let response: Response;
    try {
      const csrf = this.tokens.get();
      response = await this.fetcher.call(globalThis, `${this.baseUrl}${path}`, {
        ...init,
        credentials: "same-origin",
        headers: {
          accept: "application/json",
          "x-request-id": requestId(),
          ...(csrf ? { "x-csrf-token": csrf } : {}),
          ...init?.headers,
        },
      });
    } catch (error) {
      throw unavailableError(error);
    }
    if (!response.ok && !(allowServiceUnavailable && response.status === 503)) {
      throw await responseError(response);
    }
    return response.json() as Promise<T>;
  }
}

function jsonPost(body: unknown): RequestInit {
  return { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) };
}

function jsonRequest(method: string, body: unknown): RequestInit {
  return { method, headers: { "content-type": "application/json" }, body: JSON.stringify(body) };
}

function pathParam(template: string, name: string, value: string): string {
  return template.replace(`{${name}}`, encodeURIComponent(value));
}

function reviewPath(template: string, projectId: string, assetId: string): string {
  return template
    .replace("{projectId}", encodeURIComponent(projectId))
    .replace("{assetId}", encodeURIComponent(assetId));
}

function queryString(values: object): string {
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(values)) {
    if (value !== undefined && value !== null && value !== "") query.set(key, String(value));
  }
  const encoded = query.toString();
  return encoded ? `?${encoded}` : "";
}

async function responseError(response: Response): Promise<ProjectClientError> {
  let body: ErrorBody = {};
  try {
    body = await response.json() as ErrorBody;
  } catch {
    // Preserve status and response request ID when a proxy returns non-JSON.
  }
  const code = body.code ?? `http_${response.status}`;
  const requestIdValue = body.requestId || response.headers.get("x-request-id");
  if (code === "bootstrap_not_found_or_expired" || code === "bootstrap_already_used") {
    return new ProjectClientError(
      "server_error",
      "That bootstrap code is invalid, expired, or has already been used.",
      code,
      requestIdValue,
      response.status,
    );
  }
  if (code === "session_invalid" || response.status === 401) {
    return new ProjectClientError("session_expired", "Your session has expired. Sign in again.", code, requestIdValue, response.status);
  }
  if (code === "device_revoked") {
    return new ProjectClientError("credential_revoked", "This credential has been revoked.", code, requestIdValue, response.status);
  }
  if (code === "csrf_mismatch") {
    return new ProjectClientError("csrf_failure", "Your security token is stale. Sign in again.", code, requestIdValue, response.status);
  }
  return new ProjectClientError("server_error", "The server could not complete the request.", code, requestIdValue, response.status);
}

function unavailableError(_error: unknown): ProjectClientError {
  return new ProjectClientError(
    "server_unavailable",
    "The server is unavailable. Check the private network connection and try again.",
    "server_unavailable",
    null,
    null,
  );
}

function requestId(): string {
  const browserCrypto = globalThis.crypto;
  const random = typeof browserCrypto?.randomUUID === "function"
    ? browserCrypto.randomUUID().replaceAll("-", "")
    : typeof browserCrypto?.getRandomValues === "function"
      ? Array.from(browserCrypto.getRandomValues(new Uint8Array(16)), (byte) => byte.toString(16).padStart(2, "0")).join("")
      : `${Date.now().toString(16)}${Math.random().toString(16).slice(2)}`;
  return `web_${random}`;
}
