# Task 9: Authentication and Secure Transports

**Status:** Planned
**Depends on:** Task 2
**Unblocks:** Tasks 10, 13, 16, 17, and 18

## Objective

Protect the server with one-time bootstrap, browser sessions, revocable desktop credentials, and trusted HTTPS access through both the local LAN and Tailscale.

## User outcome

The same private companion server is securely reachable in Chrome and by the Mac at home or away, without exposing it publicly or maintaining a general user-account system.

## Scope

- Add authentication/session/device credential tables and services.
- Add local-only one-time bootstrap-code creation and exchange.
- Add browser session creation, renewal policy, logout, listing, and revocation.
- Add desktop pairing-code exchange and revocable device credentials.
- Store desktop credentials in macOS Keychain through a thin Tauri adapter.
- Add CSRF, origin/host validation, secure headers, request limits, and authentication middleware.
- Add Caddy LAN HTTPS configuration using an internal CA.
- Add Tailscale Serve setup targeting a loopback-only server endpoint.
- Add trusted proxy configuration and forwarded-header validation.
- Add redacted authentication and transport diagnostics.

## Out of scope

- Multiple accounts or roles
- Password reset/email/SSO
- Public Internet access or Tailscale Funnel
- Using Tailscale identity headers as the only authorization check
- Desktop baseline pairing workflow beyond credential exchange
- General secrets manager

## Credential model

### Browser

1. A local server command emits a short-lived one-time bootstrap code.
2. Chrome submits the code over a trusted HTTPS route.
3. The server invalidates the code and issues an opaque session cookie.
4. The cookie is HttpOnly, Secure, SameSite=Strict, scoped to the current host, and paired with CSRF protection.
5. LAN and tailnet hostnames are distinct origins and therefore establish separate browser sessions.

### Desktop

1. An authenticated browser/admin command creates a short-lived pairing code.
2. The Mac exchanges it over HTTPS for an opaque device token bound to the expected server/node identity.
3. The server stores only a salted verifier and credential metadata.
4. The Mac stores the plaintext token in Keychain and only a reference in desktop settings.
5. Revocation causes the next request to fail with an explicit `credential_revoked` error.

## Transport topology

- Server application backend: loopback host publication plus private Compose network only.
- Tailscale: host `tailscale serve` privately proxies the loopback backend with tailnet HTTPS.
- LAN: Caddy proxies the private backend and issues a certificate for a configurable local hostname through its internal CA.
- Setup exports the Caddy root certificate and documents installing/trusting it on the one Mac/Chrome client.
- Plaintext LAN access is disabled by default and cannot accept authenticated browser or desktop requests.

## Design requirements

1. Bootstrap codes and plaintext device tokens are displayed once, expire, and never enter logs.
2. Credential material uses cryptographically secure randomness and slow/salted verifier hashing appropriate for opaque tokens.
3. Authentication checks apply equally to LAN and Tailscale routes.
4. Backend trust of forwarded headers is limited to configured loopback/private proxy sources.
5. CORS is same-origin by default; wildcard origins are forbidden.
6. Cookie mutations require CSRF validation in addition to SameSite policy.
7. Media endpoints do not place bearer tokens in query parameters.
8. Bootstrap/login endpoints have bounded body size and rate limiting.
9. Tailscale Funnel is absent from deployment files and validation fails if public exposure is configured by the managed setup.
10. Session/device listing returns metadata and last use, not secret values.
11. Revoking one browser session or device token does not revoke unrelated credentials.
12. Server readiness reports insecure/misconfigured public bindings without leaking credentials.
13. The desktop remains locally usable when its credential expires or is revoked.

## Likely implementation areas

- `core/rcdvlog-server` auth domain and persistence
- `apps/server` middleware and auth routes
- `apps/desktop/src-tauri` Keychain adapter and credential exchange command
- `deploy/server/Caddyfile`, Compose, and Tailscale helper/configuration
- Security and integration tests

## Acceptance criteria

- [ ] First boot is inaccessible for product APIs until a valid bootstrap exchange establishes a session.
- [ ] Bootstrap code is single-use, expires, and is absent from logs/database plaintext.
- [ ] Browser session supports authenticated request, CSRF-protected mutation, logout, and revocation.
- [ ] Desktop pairing token authenticates sync endpoints and is stored in Keychain rather than settings/catalog files.
- [ ] Revoked credentials receive a stable error on LAN and Tailscale routes.
- [ ] Spoofed Tailscale/forwarded identity headers sent directly to the backend grant no access.
- [ ] Invalid Host, Origin, CSRF, body size, and rate-limit cases fail safely.
- [ ] Caddy serves trusted LAN HTTPS after installing its local CA on Chrome.
- [ ] Tailscale Serve reaches the same backend privately with tailnet HTTPS.
- [ ] Plain HTTP and direct unauthenticated backend access cannot perform authenticated operations.
- [ ] No deployment file enables Funnel or binds the raw backend publicly.
- [ ] Redacted diagnostics identify route, certificate, session, and proxy configuration without secrets.

## Verification

Run unit/integration security tests and a Compose smoke test. Manually validate Chrome over both configured LAN and tailnet hostnames, including separate sessions and device revocation. Inspect logs and diagnostic bundles for secret leakage.

## Handoff notes

Document middleware requirements for every later API route. No task may add an unauthenticated exception beyond explicit liveness and the tightly scoped bootstrap exchange.
