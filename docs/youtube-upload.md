# YouTube uploads

The `rcdvlog youtube` commands provide upload-only OAuth, persistent credentials,
resumable uploads, and local history. They are opt-in and work without the desktop
or companion server. They do not schedule renders or install an always-running
uploader. A successful render pipeline can invoke `youtube upload` as its last
step; a nonzero render result must prevent that step.

## One-time Google setup

1. Create a dedicated project in [Google Cloud Console](https://console.cloud.google.com/).
2. Enable **YouTube Data API v3** in the API Library.
3. Configure Google Auth Platform branding, audience and data access. Add only
   `https://www.googleapis.com/auth/youtube.upload`.
4. For personal use with an external audience, set publishing status to **In
   production before authorizing**. External apps left in Testing get refresh
   tokens that expire in seven days for this scope. Production does not guarantee
   an immortal token: revocation, inactivity and Google account policies can
   still require reauthorization. Google documents a personal-use exemption from
   OAuth verification; an unverified-app warning may remain.
5. Create an OAuth client of type **Desktop app**, download its JSON, and keep it
   outside the project/repository. This is not a service-account credential.
6. Run:

```bash
rcdvlog youtube auth login --client-file /absolute/private/client.json
rcdvlog youtube auth status
```

Login opens the system browser and waits for Google's loopback callback. Select
both the correct Google account and the intended YouTube/Brand channel, then
approve the upload permission. The listener binds only `127.0.0.1` on an ephemeral
port by default and closes when login succeeds, fails, or times out (ten minutes
by default; `--timeout-seconds` permits 1–3600 seconds).

The authorization request uses S256 PKCE, random state, offline access, explicit
consent and account selection, and disables inclusion of previously granted
scopes. Token responses must grant exactly `youtube.upload`; a broader grant is
rejected. A dedicated Google project/client avoids old grants on a reused app.
There is no request for email, profile, channel read access, or general YouTube
management. The uploader uses the upload response's video/channel IDs and does
not poll processing status using additional permissions.

### Agent-led and headless authorization

An agent can launch the following command as a subprocess:

```bash
rcdvlog youtube auth login \
  --client-file /absolute/private/client.json \
  --no-browser --port 8765 --timeout-seconds 600
```

It emits a `youtube.oauth.authorization_required` JSON event on stderr containing
`authorizationUrl`, `redirectUri`, `port`, expiry, requested scope and a suggested
SSH forward. The process stays running. The agent can present the URL while the
user signs in and grants consent. Do not send authorization codes or tokens
through agent chat.

When the upload host is remote, run this on the computer with the browser,
substituting the host name, then open the printed URL on that computer:

```bash
ssh -N -L 8765:127.0.0.1:8765 your-upload-host
```

The forward and `--port` must match. A busy listener port fails with a bind error;
there is no fallback port. This flow needs a desktop browser with a reachable
loopback callback; opening the URL on a phone alone does not forward localhost
back to the upload host. Google's obsolete out-of-band copy/paste flow is not
used. A phone-friendly public HTTPS callback would need a separate web-client
integration and is not implemented.

After the callback, the command exchanges the code and writes the credential,
then emits its final stdout result. Browser acknowledgement only confirms receipt
of the callback; wait for the CLI's final successful result to confirm storage.
Login failures preserve the prior credential. Reauthorizing generates a new
credential identity to avoid resuming a previous channel's upload accidentally.
History deduplication is scoped to that identity, so inspect history/Studio before
uploading previously published files after reauthorizing.

### Credential storage and revocation

macOS defaults to the `com.rcd.vlogcutter.youtube` Keychain service. Linux defaults
to `<local data dir>/com.rcd.vlogcutter/youtube/credentials.json` (normally
`~/.local/share/com.rcd.vlogcutter/youtube/credentials.json`). For headless hosts,
an explicit owner-only file is supported on either platform:

```bash
export RCDVLOG_YOUTUBE_CREDENTIAL_FILE=/absolute/private/youtube-credentials.json
# Alternatively pass --credential-file anywhere after "youtube".
```

Credential files and their persistent lock files must be owned by the current
user with no group/other permissions (`0600`). Newly created private credential
directories use `0700`; existing parent-directory permissions are not changed.
Symlink credential files are refused. Writes use a same-directory private staging
file, fsync and atomic rename. Access/refresh tokens, Desktop client credentials
and PKCE verifiers never appear in JSON results or errors. Endpoint URLs in
client JSON are ignored: production requests use fixed Google endpoints, and
HTTP redirects are disabled. Refresh responses without a replacement refresh
token preserve the saved token.

`auth status` is an offline inspection: it does not contact Google, refresh a
token, open a catalog or create directories. `authorized: true` means a valid
local credential exists, not that Google has just accepted it. Uploads refresh
access tokens automatically; `youtube.auth_expired` requires a new login.

```bash
rcdvlog youtube auth revoke
```

Revoke asks Google to revoke the grant and deletes the local credential after
confirmation. A failed revocation retains it for retry. Google revocation can
also affect other tokens on the same app grant. A credential lock prevents login,
refresh, revocation and upload from racing; another writer reports
`youtube.credential_busy` immediately. Status remains readable during an upload.

The Google scope restricts API access, not which local files can be uploaded or
whether an upload can be public. Those are caller policy choices.

## Upload a completed cut

Upload/history require an **explicit existing catalog** via `--catalog` or
`RCDVLOG_CATALOG` and an explicit project ID. Upload performs a forward catalog
migration if needed; it never creates a missing catalog. Auth commands need no
catalog. After upgrade, older builds that support only schema 48 will refuse
writable access; keep using the current build for that catalog. Build with `cargo +1.97.1 build --locked -p rcdvlog-cli`.

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 youtube upload \
  --project project_example \
  --file /absolute/project/exports/rough-cut-2.mp4 \
  --title 'Antarctica — Rough Cut 2' \
  --description-file /absolute/project/exports/youtube-description.txt \
  --cut-revision editrev_example \
  --privacy private
```

Description defaults to empty. Visibility always defaults explicitly to private;
`unlisted` and `public` require the corresponding option. Category is People &
Blogs (`22`). No made-for-kids or synthetic-media declaration is inferred; the
channel's applicable defaults apply. A cut revision is an optional caller-supplied
provenance label, not a verification that the video was rendered from that edit
revision. The exact bytes are identified by a full-file SHA-256.

The command hashes the completed file, starts a resumable session, and sends
bounded 8 MiB chunks. Keep the render immutable: changes to its size or modification
time abort the transfer. Progress events include accepted bytes, total bytes and
a one-worker shared batch estimate; successful transfers update persisted rate
history. Bounded retries handle network errors, selected server failures and
throttling. The saved session is queried before continuing, including after a lost
final response. A refreshed token is retried after a 401. Production session URLs
must have the exact Google upload origin and path before any token is sent.

Catalog schema 49 adds the **local, unsynced** `youtube_uploads` ledger. It records
project/credential identity, file hash/size/path, metadata, revision label, accepted
bytes, session state, video/channel IDs and timestamps. It stores no OAuth token.
The resumable session URI is retained only while transferring and omitted from
serialized/debug history and errors; treat the catalog as private operational
state. Persistent local artifact locks and a uniqueness constraint prevent
competing invocations from creating duplicate records. This is not global
idempotency across independently copied catalogs or freshly minted credentials.

On an ordinary rerun:

- A completed record returns its URL with `reused: true`, without uploading again.
- An interrupted transfer probes its saved session and continues from Google's
  accepted offset. Keep the same title, description, visibility and revision.
- Changed metadata for the same bytes reports `youtube.metadata_conflict`; this
  command does not edit existing videos. Edit details in YouTube Studio or upload
  a new rendered artifact.
- A lost session-creation response reports `youtube.outcome_unknown`; an expired
  session reports `youtube.session_expired`. Neither automatically creates a new
  session. Inspect Studio before using `--restart-upload`, which explicitly
  permits a replacement and may create a duplicate.

The successful result contains `upload`, `url`, `reused`, and
`processingVerified: false`. Success means YouTube accepted the bytes and returned
a video ID; it does not certify processing completion, playback or visibility.
Check the resulting video in Studio. Google currently has conflicting official
documentation about visibility restrictions on unaudited API projects; verify
unlisted/public behavior before relying on it for sharing. OAuth app publishing
status and a YouTube API compliance audit are separate concerns.

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 youtube history \
  --project project_example --limit 20
```

History is read-only, needs no credentials, and never contacts Google. It requires
a catalog already migrated to schema 49. To automate after a render, invoke upload
only after the renderer and its output checks succeed. CLI success is one JSON
envelope on stdout; progress is newline-delimited JSON on stderr. Failure keeps
stdout empty and ends stderr with one JSON error envelope (possibly after progress
or authorization events).

## Validation and remaining acceptance

Local tests use fake credentials, temporary catalogs and actual loopback HTTP
fixtures for OAuth/token refresh, callback state/Host/Origin checks, private file
storage, revocation, chunked resume after reopening the catalog, lost final
responses, duplicate avoidance, expired-session recovery and session-origin
validation. CLI process tests exercise JSON, explicit catalog selection,
observational status/history and headless authorization timeout. Existing
migration tests upgrade catalogs stopped at every previous schema version.

These tests do not establish real Google consent, Brand channel selection,
macOS Keychain behavior, YouTube processing, or unlisted/public visibility.
Authorize a real Desktop client, upload a private fixture, inspect Studio and
verify rerun reuse before enabling unattended uploads of real cuts.

References:

- [Google desktop OAuth and PKCE](https://developers.google.com/identity/protocols/oauth2/native-app)
- [Refresh token expiration](https://developers.google.com/identity/protocols/oauth2/)
- [Personal-use verification exemption](https://support.google.com/cloud/answer/13464323?hl=en)
- [YouTube videos.insert](https://developers.google.com/youtube/v3/docs/videos/insert)
- [Resumable upload protocol](https://developers.google.com/youtube/v3/guides/using_resumable_upload_protocol)
- [Visibility documentation clarification](https://issuetracker.google.com/issues/572863050)
