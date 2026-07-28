# Tailscale Serve Setup for RCD VlogCutter

This document describes how to expose the private companion server through
Tailscale so the Mac desktop can reach it when away from the home LAN.

## Prerequisites

- Tailscale installed and authenticated on both the Ubuntu server and the Mac.
  https://tailscale.com/download
- The server process must already be running (listen on `127.0.0.1:8090`).

## 1. Verify Tailscale is running

```bash
tailscale status
```

Expected: both the server and Mac appear with their tailnet hostnames.

## 2. Enable Tailscale Serve (tailnet HTTPS)

Tailscale Serve proxies the loopback HTTP backend and provides a private
tailnet HTTPS certificate:

```bash
tailscale serve --bg --https 8443 http://127.0.0.1:8090
```

- `--bg` runs the proxy in the background.
- `--https 8443` serves HTTPS on port 8443 (the server backend stays on 8090).
- The certificate is automatically provisioned by Tailscale.

## 3. Verify the proxy is active

```bash
tailscale serve status
```

Expected output includes an explicit `Funnel off` status:

```
# Funnel off
# TCP :443
# TCP :8443
https://<server-hostname>.<tailnet>.ts.net:8443  -> http://127.0.0.1:8090
```

## 4. Confirm Funnel is NOT enabled

```bash
tailscale serve status | grep -i funnel
```

The status must explicitly report Funnel off. If it reports an active Funnel, disable it immediately:

```bash
tailscale funnel off
```

Tailscale Funnel would expose the server to the public Internet, which is
forbidden by RCD VlogCutter's security model.

## 5. Access the server

From the Mac (anywhere with Tailscale connected):

```bash
curl https://<server-hostname>.<tailnet>.ts.net:8443/api/v1/health
```

The server version endpoint is also public:

```bash
curl https://<server-hostname>.<tailnet>.ts.net:8443/api/v1/version
```

## 6. Desktop pairing

Once the server is reachable, use the desktop's pairing command. The endpoint
to provide during pairing is:

```
https://<server-hostname>.<tailnet>.ts.net:8443
```

The desktop stores the device token in the macOS Keychain and keeps the
server identity for future sync.

## 7. Credential revocation

If the Mac is lost or credentials need to be rotated:

1. Open Chrome and navigate to the server's LAN or tailnet URL.
2. Authenticate with a valid browser session.
3. Go to the admin dashboard and revoke the desktop's device credential.
4. On the Mac, unpair the server and re-pair with a new pairing code.

## Troubleshooting

| Symptom | Check |
|---------|-------|
| `tailscale serve` fails | Ensure the server is running on `127.0.0.1:8090` |
| Certificate errors on Mac | Reconnect to tailnet: `tailscale down && tailscale up` |
| `connection refused` | Verify the server health endpoint responds on loopback |
| Funnel is enabled | Run `tailscale funnel off` immediately |
