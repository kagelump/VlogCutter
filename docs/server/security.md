# HTTPS, bootstrap, pairing, and credentials

The server has two supported private HTTPS origins: Caddy on the home LAN and Tailscale Serve on the tailnet. They proxy the same loopback backend and stable server node ID. They are separate browser origins, so each needs its own browser session. Tailscale identity is not a substitute for VlogCutter authentication.

## LAN TLS with Caddy

Install Caddy on the Ubuntu host, copy [`deploy/server/Caddyfile`](../../deploy/server/Caddyfile) to `/etc/caddy/Caddyfile`, ensure `rcdvlog.local` resolves to the server on the Mac, and start or reload Caddy. Copy Caddy's local CA certificate from the Ubuntu host to the Mac through a trusted channel, add it to the macOS System keychain, and set it to Always Trust. Do not copy the CA private key.

Verify from the Mac:

```bash
curl --fail https://rcdvlog.local:8443/api/v1/version
```

For tailnet HTTPS, follow [`deploy/server/tailscale-setup.md`](../../deploy/server/tailscale-setup.md). Never enable Funnel. The backend trusts forwarded headers only from configured loopback proxy CIDRs, validates Host/Origin/forwarded combinations, and rejects foreign WebSocket origins.

## First browser session

Bootstrap creation is deliberately server-local and offline. With the checked-in Compose deployment, prepare one host directory for owner-only admin output, stop the service, run the same image against its named data volume, and restart:

```bash
sudo install -d -o 10001 -g 10001 -m 700 /srv/rcdvlog/admin-output
docker compose --env-file deploy/server/.env -f deploy/server/compose.yaml stop rcdvlog-server
docker compose --env-file deploy/server/.env -f deploy/server/compose.yaml run --rm --no-deps \
  --volume /srv/rcdvlog/admin-output:/run/rcdvlog-admin \
  rcdvlog-server admin bootstrap create \
  --output-file /run/rcdvlog-admin/bootstrap-code.txt
docker compose --env-file deploy/server/.env -f deploy/server/compose.yaml up -d
```

Read `/srv/rcdvlog/admin-output/bootstrap-code.txt` locally, open the trusted HTTPS origin in Chrome, and exchange the code on the sign-in screen. The code is single-use. The session credential is an HttpOnly, Secure, SameSite=Strict cookie; a separate same-origin CSRF value supports concurrent tabs. Do not put codes, cookies, device tokens, or CSRF values in URLs, shell history, logs, screenshots, or issue text. Delete the bootstrap file after exchange. A rerun must use a new output filename because the command never overwrites a file.

## Pair a Mac and adopt a project

In Chrome, open Settings → Devices and create a pairing code (default TTL: 600 seconds). In the desktop app, open Settings, choose “Connect to server…”, enter the HTTPS endpoint, verify the displayed server identity, and paste the pairing code. The plaintext device token is stored in macOS Keychain; settings store only non-secret endpoint and node metadata.

For an existing project, choose “Pair with server”, review the path-free baseline preview and explicit logical-root mappings, then confirm. Baseline adoption transfers operations, facts, observations, and representation manifests—not originals or arbitrary absolute paths. A selected project can pair to only one stable server ID.

## Rotation and revocation

Browser Settings lists sessions and paired devices without their secrets. Revoke stale sessions or devices individually. Revoking the active browser session signs that session out; revoking a Mac device makes the next sync request fail while all local desktop work remains usable. To rotate a desktop credential, revoke it, disconnect the desktop, create a new pairing code, and pair again. Re-adopting project state is not normally required when the stable server/project pairing is unchanged.

Agents use `RCDVLOG_SERVER_TOKEN` or an explicit owner-only credential file. Prefer the file so secrets do not enter ambient process environments. Never pass a token as a command-line argument. See [agent-cli.md](../agent-cli.md).

## Security limits

Open credential exchanges are limited to 64 KiB and 12 attempts per minute per in-process window. The limiter resets on restart and is not an internet-facing abuse system. CSP, restrictive Permissions-Policy, same-origin rules, request limits, path containment, opaque media IDs, and authenticated byte-range streaming are enforced, but the LAN/tailnet boundary and ACLs remain required.
