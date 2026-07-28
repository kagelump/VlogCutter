# Review: Tasks 9–12

> Historical point-in-time review. It is preserved as delivery evidence and is superseded by the current [`docs/server`](../../server/README.md) guidance, final task statuses, and Task 19 acceptance report.

**Date:** 2026-07-24
**Scope:** Authentication & Secure Transports (T9), Desktop Pairing & Root Mapping (T10), Automatic Sync & Conflict Handling (T11), Remote Representation Exchange & MAC Requests (T12)

---

## Summary

11 findings across the four tasks, 7 fixed, 4 deferred. One critical CSRF bypass was the only finding that made client-side protections ineffective; the rest are correctness, performance, or robustness improvements.

---

## Findings & Fixes

### 1. CRITICAL — CSRF Bypass (`apps/server/src/lib.rs`)

`require_csrf` passed an empty `session_token` to `validate_session_with_csrf`, so the method failed before reaching the CSRF comparison. Mutations (revoke sessions, create pairing codes, delete devices) were accepted with any non-empty CSRF header.

**Fix:** Added `AuthService::validate_csrf_token(session_id, csrf_token)` that compares only the CSRF hash, and wired `require_csrf` to call it.

### 2. HIGH — O(n) Argon2 Scan per Device Auth (`core/rcdvlog-server/src/auth.rs`)

`validate_device` loaded every non-revoked credential and performed an Argon2 verification on each one. With N paired devices this is N memory-hard hashes per request — a CPU-exhaustion DoS vector.

**Fix:** Added `WHERE token_prefix = ?1` to the SQL query so the server only tests credentials matching the 8-character prefix (at most a handful).

### 3. MEDIUM — Trailing-Slash Keychain Mismatch (`apps/desktop/src-tauri/src/lib.rs`)

`pair_with_server` stored the endpoint verbatim and `read_server_credentials` looked it up the same way. A URL entered with a trailing slash (`https://host:8443/`) produced a different Keychain entry than the same URL without one.

**Fix:** Both functions now trim trailing slashes before storing or looking up.

### 4. MEDIUM — Asymmetric Backoff Jitter (`apps/desktop/src-tauri/src/sync_manager.rs`)

The jitter formula `delay + 0.25 * delay` was always positive, so every retry landed ~25% later than intended. Symmetric jitter (±25%) spreads load evenly and converges faster.

**Fix:** Changed to `delay + rand::thread_rng().gen_range(-jitter..=jitter)`.

### 5. LOW — Misleading 403 Error Message (`apps/desktop/src-tauri/src/server_client.rs`)

All HTTP 403 responses (including `csrf_mismatch`, `bootstrap_already_used`, `pairing_already_used`) were reported as "device credential revoked," steering users toward re-pairing unnecessarily.

**Fix:** Message changed to "Check that the device credential is still valid."

---

## Deferred Findings

| # | Finding | Reason |
|---|---------|--------|
| 6 | Recursive `Box::pin` pagination in `reconcile_project` (sync_manager.rs:623) | Requires structural refactor — deep stacks and borrow-checker fragility under load. Recommended as the next maintenance item. |
| 7 | Dynamic JSON parsing in `pair_with_server` (lib.rs:801) | Typed deserialization would catch schema drift at compile time, but the current `.ok_or()` pattern is functionally equivalent for the existing response shape. |
| 8 | Byte-index slicing panic risk (auth.rs:207) | Input comes from `generate_alphanumeric` (guaranteed ASCII); switching to `.get(..N)` would mask programming errors in other callers. |
| 9 | `propose_root_mappings` no canonicalization (lib.rs:984) | `fs::canonicalize` has I/O side effects and fails on non-existent paths; canonicalization belongs at the caller level. |

---

## Files Touched by Fixes

| File | Change |
|------|--------|
| `apps/server/src/lib.rs` | CSRF bypass fix |
| `core/rcdvlog-server/src/auth.rs` | CSRF token validation method + token-prefix SQL filter |
| `apps/desktop/src-tauri/src/lib.rs` | Trailing-slash normalization in two functions |
| `apps/desktop/src-tauri/src/sync_manager.rs` | Symmetric jitter + `use rand::Rng` |
| `apps/desktop/src-tauri/Cargo.toml` | Added `rand.workspace = true` |
| `apps/desktop/src-tauri/src/server_client.rs` | 403 error message |
