import { HttpProjectClient } from "@rcdvlog/project-client/http";
import { ProjectClientError } from "@rcdvlog/project-client";

function response(body: unknown, status = 200, headers: Record<string, string> = {}) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

test("preserves CSRF failures and request IDs", async () => {
  const fetcher = vi.fn().mockResolvedValue(response(
    { code: "csrf_mismatch", requestId: "request_csrf" },
    403,
  ));
  const client = new HttpProjectClient("https://server.test", fetcher);

  await expect(client.getSession()).rejects.toMatchObject({
    kind: "csrf_failure",
    code: "csrf_mismatch",
    requestId: "request_csrf",
  } satisfies Partial<ProjectClientError>);
});

test("reports protocol mismatch separately", async () => {
  const fetcher = vi.fn()
    .mockResolvedValueOnce(response({
      requestId: "request_version",
      applicationVersion: "2.0.0",
      protocolVersion: { major: 2, minor: 0 },
      controlSchemaVersion: 8,
      nodeId: "node_newer",
    }))
    .mockResolvedValueOnce(response({
      requestId: "request_ready",
      status: "ready",
      checks: [],
    }));
  const client = new HttpProjectClient("https://server.test", fetcher);

  await expect(client.getServerStatus()).rejects.toMatchObject({
    kind: "protocol_mismatch",
    requestId: "request_version",
  } satisfies Partial<ProjectClientError>);
});

test("treats an invalid session as unauthenticated bootstrap state", async () => {
  const fetcher = vi.fn().mockResolvedValue(response(
    { code: "session_invalid", requestId: "request_session" },
    401,
  ));
  const client = new HttpProjectClient("https://server.test", fetcher);

  await expect(client.getSession()).resolves.toEqual({
    authenticated: false,
    sessions: [],
  });
});

test("reports a network failure as server unavailable", async () => {
  const fetcher = vi.fn().mockRejectedValue(new TypeError("offline"));
  const client = new HttpProjectClient("https://server.test", fetcher);

  await expect(client.getServerStatus()).rejects.toMatchObject({
    kind: "server_unavailable",
    code: "server_unavailable",
  } satisfies Partial<ProjectClientError>);
});
