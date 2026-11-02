import { ProjectClientError } from "@rcdvlog/project-client";

export function InlineError({ error }: { error: ProjectClientError }) {
  return <p className="inline-error" role="alert">{error.message}{error.requestId ? ` Request ID: ${error.requestId}` : ""}</p>;
}

export function normalizeError(error: unknown): ProjectClientError {
  return error instanceof ProjectClientError
    ? error
    : new ProjectClientError("server_error", "The application could not complete the request.", "unexpected_error");
}
