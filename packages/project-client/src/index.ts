export const SUPPORTED_PROTOCOL_MAJOR = 1;

export interface ProtocolVersion {
  major: number;
  minor: number;
}

export interface ServerVersion {
  requestId: string;
  applicationVersion: string;
  protocolVersion: ProtocolVersion;
  controlSchemaVersion: number;
  nodeId: string;
}

export interface ReadinessCheck {
  name: string;
  status: "ok" | "error";
  code?: string;
}

export interface ServerReadiness {
  requestId: string;
  status: "ready" | "not_ready";
  checks: ReadinessCheck[];
}

export interface ServerStatus {
  version: ServerVersion;
  readiness: ServerReadiness;
}

export interface SessionInfo {
  sessionId: string;
  displayPrefix: string;
  hostname: string;
  createdAt: string;
  lastUsedAt: string;
  expiresAt: string;
  revoked: boolean;
}

export interface SessionState {
  authenticated: boolean;
  sessions: SessionInfo[];
}

export interface LoginResult {
  requestId: string;
  sessionId: string;
  displayPrefix: string;
  expiresAt: string;
}

export type ClientErrorKind =
  | "server_unavailable"
  | "session_expired"
  | "credential_revoked"
  | "csrf_failure"
  | "protocol_mismatch"
  | "server_error";

export class ProjectClientError extends Error {
  constructor(
    public readonly kind: ClientErrorKind,
    message: string,
    public readonly code: string,
    public readonly requestId: string | null = null,
    public readonly status: number | null = null,
  ) {
    super(message);
    this.name = "ProjectClientError";
  }
}

/** Operations available to either a native or HTTP-backed project surface. */
export interface ProjectClient {
  getServerStatus(): Promise<ServerStatus>;
}

/** Browser-session operations. Native clients authenticate with device credentials instead. */
export interface BrowserProjectClient extends ProjectClient {
  getSession(): Promise<SessionState>;
  loginWithBootstrapCode(code: string, hostname?: string): Promise<LoginResult>;
}

export function assertProtocolCompatible(version: ServerVersion): void {
  if (version.protocolVersion.major !== SUPPORTED_PROTOCOL_MAJOR) {
    throw new ProjectClientError(
      "protocol_mismatch",
      `This web app supports protocol ${SUPPORTED_PROTOCOL_MAJOR}, but the server uses ${version.protocolVersion.major}.`,
      "protocol_mismatch",
      version.requestId,
    );
  }
}

export * from "./generated";
