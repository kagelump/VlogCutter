# Same-machine CLI catalogs

An attached project makes the web app a view into the CLI's existing working catalog. Clip metadata, descriptions, notes and labels are read from and written to that same SQLite catalog. The server retains its separate control database for review targets, recordings, transcripts, authentication and jobs. Originals remain read-only.

## Attach a catalog

Use the same build for the CLI and server, with the catalog and both processes on the same local filesystem. Do not share SQLite/WAL over NAS, cloud drives or across machines. Configure `RCDVLOG_ATTACHED_PROJECT_ROOTS` as a comma-separated list of absolute existing directories containing the catalogs and thumbnail caches you intend to expose. The default is empty: attachment and local thumbnail access are disabled. Keep the allowlist narrow and exclude originals and private unrelated directories.

Stop the server gracefully, then run its offline admin command with the same environment:

```bash
export RCDVLOG_ATTACHED_PROJECT_ROOTS=/absolute/path/to/working-project
rcdvlog-server admin projects attach --catalog /absolute/path/to/working-project/catalog.sqlite3
```

Pass `--project-id <id>` if a catalog contains several projects. The command emits one JSON envelope; errors use `attached_catalog_refused` or the existing administration error codes. Start the server again with the same allowlist. The catalog must have exactly the schema this build supports. The server refuses mismatched schemas without migrating, recovering or redispatching the CLI catalog. Upgrade deliberately through the CLI while the server is stopped. The allowlist and schema are checked again when the server opens the catalog. Its existing local node identity is preserved, so CLI and HTTP writes share one operation stream and SQLite's locking.

An existing same-ID replica is retained and its registry entry is redirected, preserving review-target/session associations. Baseline import and reconcile refuse attached projects with `project_attached` (HTTP 409). Attachments do not create a second canonical timeline.

## Thumbnails and descriptions

The Clips and Shots pages read analysis descriptions from the catalog. `GET` and `HEAD /api/v1/projects/{projectId}/assets/{assetId}/thumbnail` serve the highest-quality recorded JPEG inside an allowed root, excluding server state, temporary and backup directories. Authentication remains required. Symlinks resolving outside allowed roots, missing files, non-JPEG paths and files over 20 MiB return 404. Responses use private caching and ETags. No browser API returns local catalog/cache paths. This serves existing thumbnails; generating missing thumbnails remains the CLI's explicit preflight/media step.

A thumbnail represents the source clip, not a newly decoded frame at a particular cut's in-point. For playback, the server uses an available server-managed preview or the catalog's existing raw-v3 MP4 proxy (or an unrotated legacy baked MP4 proxy) through authenticated `GET`/`HEAD …/assets/{assetId}/proxy`, with single byte ranges, ETags and private revalidation for seeking and replacement. Add the proxy cache directories to the same explicit allowlist. Only attached catalogs qualify; unsupported rotated legacy recipes, missing/empty files, originals, escaped symlinks and files under server state/temp/backup roots are refused. Proxy and original availability are read from the local catalog even without a global media-library link. Missing proxies are not generated implicitly. CLI edits appear on a fresh request or page reload. Cross-process push notifications are not implemented.

## Rough cuts and recovery

A working project can contain multiple immutable edit revisions. Treat each rough cut as a snapshot of its selected timeline revision. Existing verified review targets remain bound to their exact revision/artifact identity; attaching does not automatically create or import new review targets. Continue using the existing verified export/import flow for those snapshots.

Backups use SQLite's online backup API to snapshot the active attached catalog alongside server state. Restore always creates a local replica under the new data root and clears attachments: it never reconnects to or modifies the original working catalog. Media files are not included. Keep independent project/media backups too.

To return to the retained replica, stop the server and run:

```bash
rcdvlog-server admin projects detach --project-id <id>
```

Detach is refused for a project originally attached without a replica. Attaching a different compatible catalog is supported. Returning to an old replica does not merge CLI changes made since attachment; take a backup and decide how to reconcile before using it.
