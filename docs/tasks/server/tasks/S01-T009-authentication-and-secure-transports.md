# S01-T009: Authentication and Secure LAN/Tailscale Transport

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T009",
  "epic_id": "S01",
  "title": "Authentication and Secure LAN/Tailscale Transport",
  "slug": "authentication-and-secure-transports",
  "status": "complete",
  "task_class": "foundation",
  "depends_on": [
    "S01-T002"
  ],
  "principal_invariant": "Every server endpoint — on LAN and Tailscale — requires application-level authentication, HTTPS is enforced on both routes, and no credential appears in a URL, query string, or log line.",
  "central_falsification": "Attempt to access a protected endpoint without a credential (verify 401), with a revoked credential (verify 401), over plain HTTP on LAN (verify redirect/reject), and verify that no access log line contains a credential. Then revoke a credential mid-session and verify the next request is rejected.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Auth middleware → all protected routes",
    "Bootstrap → device credential lifecycle"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/OPERATIONS.md"
    ],
    "implementer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/OPERATIONS.md"
    ],
    "independent_tester": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/OPERATIONS.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/server/tasks/S01-T009-authentication-and-secure-transports.md"
    ],
    "implementer": [
      "apps/server/src/**",
      "core/rcdvlog-server/src/**"
    ],
    "independent_tester": [
      "apps/server/tests/**"
    ],
    "integrator": [
      "deploy/server/Caddyfile",
      "deploy/server/compose.yaml",
      "deploy/server/tailscale-setup.md"
    ]
  },
  "forbidden_paths": [
    "core/rcdvlog-core/src/catalog/**",
    "apps/desktop/**",
    "apps/web/src/**",
    "apps/mobile/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-server",
    "cargo test -p rcdvlog-server --test integration",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 90 \\| \\[S01-T009\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Add application-level authentication, device credentials, browser sessions,
LAN HTTPS through Caddy, Tailscale HTTPS through Tailscale Serve, and the
bootstrap flow for first-use server setup.

## Principal invariant

This task is correct when every server endpoint — on LAN and Tailscale —
requires application-level authentication, HTTPS is enforced on both routes,
and no credential appears in a URL, query string, or log line.

## Inputs and outputs

Inputs:
- S01-T002 server HTTP listener and configuration

Outputs:
- Bootstrap flow: initial admin credential setup
- Device credential model: create, list, revoke
- Browser session management (secure cookies, expiry, CSRF protection)
- Auth middleware applied to all non-health routes
- LAN HTTPS through Caddy with internal CA
- Tailscale HTTPS through Tailscale Serve
- Strict trusted-proxy/origin configuration
- Credential audit log without secret exposure
- Same app credentials on LAN and Tailscale

## Frozen decisions

1. The server listener remains loopback-only; Caddy and Tailscale Serve sit in front.
2. Same application credentials on LAN and Tailscale — no alternate auth path.
3. Bootstrap requires physical or console access; no remote bootstrap without existing credential.
4. Device credentials are revocable individually without affecting other devices.
5. Secrets are redacted wrapper types; never emitted in logs, config diagnostics, URLs, or query strings.
6. Browser sessions use secure cookies with bounded expiry.
7. Auth failure returns 401 with stable error code, never distinguishing "no credential" from "wrong credential."

## Owned paths

- **Implementer:** `apps/server/src/` (auth middleware, bootstrap, sessions, routes), `core/rcdvlog-server/src/` (credential storage, lifecycle)
- **Independent tester:** `apps/server/tests/` with credential lifecycle and auth-bypass attempts
- **Integrator:** `deploy/server/Caddyfile`, `deploy/server/compose.yaml`, `deploy/server/tailscale-setup.md`

## Required interface

- Auth middleware consumed by all downstream route tasks (S01-T010, S01-T014, etc.)
- Pairing endpoints (`POST /api/v1/auth/pairing`, `POST /api/v1/auth/devices/exchange`) consumed by desktop and phone
- Device credential management API

## Acceptance oracle

1. Bootstrap creates initial admin credential on first start.
2. Protected endpoints reject requests without credential (401).
3. Revoked credential is rejected on next request.
4. Revocation mid-session takes effect promptly.
5. LAN requests over plain HTTP are redirected to HTTPS or rejected.
6. Tailscale requests arrive over HTTPS through Tailscale Serve.
7. Same credential works on LAN and Tailscale.
8. No credential appears in any URL, query string, log line, or persisted request.
9. Browser sessions expire and require re-authentication.
10. No authentication bypass through alternate route.

## Independent test charter

Try to falsify: "no unauthenticated access to protected routes." Enumerate every
non-health route, attempt access without credential (assert 401), with revoked
credential (assert 401), with expired session (assert 401). Attempt plain HTTP
on LAN (assert redirect/reject). Attempt to access Tailscale route without
credential. Grep all log output for credential values. Test that bootstrap
cannot be triggered remotely.

## Review charter

Risk domain: authentication integrity. Questions: (1) Can any protected route be
accessed without a credential? (2) Does revocation take effect promptly? (3) Do
credentials appear in logs, URLs, or responses? (4) Can bootstrap be triggered
remotely? (5) Is plain HTTP rejected on LAN? Severity: P0 for any authentication
bypass, credential leakage, or remote bootstrap; P1 for delayed revocation or
session mismanagement. Approval vocabulary: `APPROVE`,
`APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- OAuth, SSO, or third-party identity providers
- Multi-user roles or permissions
- Public hosting or Tailscale Funnel
- Hardware token or biometric authentication

## Stop conditions

Stop and escalate when:

- The server listener must bind to non-loopback for authentication to work
- A credential must appear in a URL or query string
- Bootstrap cannot be completed without a browser
- LAN and Tailscale need different authentication mechanisms

## Commands

```sh
cargo test -p rcdvlog-server
cargo test -p rcdvlog-server --test integration
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Auth middleware and pairing endpoints are consumed by S01-T010 (desktop pairing),
S01-T013 (web foundation), S01-T016 (web admin), T01-T002 (transfer auth),
and T01-T006 (mobile pairing).
