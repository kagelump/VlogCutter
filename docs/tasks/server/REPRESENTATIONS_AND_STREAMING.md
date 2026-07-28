# Representations and Streaming

> Historical Task 6 implementation snapshot. Current representation/streaming guidance is in [`docs/server/jobs-and-media.md`](../../server/jobs-and-media.md).

This is the implementation contract delivered by Task 6.

## Identity and persistence

Control-catalog migration 5 adds `representations` and `representation_locations`. A representation ID is deterministic over the verified full source BLAKE3 hash and byte count, representation kind, current recipe key, and an output-settings identity. Pending rows support durable requests; a representation becomes `available` only in the same transaction that completes its durable job.

Current recipes are unchanged:

- `proxy.h264-720p.orientation-v3`
- `thumbnail.q1.orientation-v2`

Only full-hash-verified media can be requested. Because identity is content- and recipe-based, project memberships do not create duplicate outputs. The job table has a partial unique index allowing at most one queued/running/retry-wait job per representation. A later request after a terminal failure creates a new durable job generation.

## Storage and atomicity

Shared outputs use this derived-root-relative layout:

```text
representations/<source-hash>/<recipe-id>/<recipe-version>/<representation-id>.<ext>
```

Every component is server-derived. Client input cannot select an output path. Temporary files are unique to a job attempt and live beside the final output so rename is atomic. A handler validates the output before rename, and the completion transaction publishes the output hash, length, and node-local location. If the server stops after rename but before the transaction, retry validates and reconciles the existing file without regenerating it.

Existing compatible project-local outputs can be registered through `register_existing_compatible_output`. Their absolute paths remain node-local database state and never appear in representation DTOs or media URLs.

## Media contracts

Proxy jobs read only verified originals and preserve the desktop v3 contract: rotation override to zero, autorotation disabled, maximum width 1280, H.264/libx264 `veryfast` CRF 23, `yuv420p`, AAC 128 kb/s when audio exists, metadata removal, and fast-start. Validation requires H.264, bounded width, `yuv420p`, AAC audio streams, and zero display rotation.

Thumbnail jobs read verified originals, seek to 20 percent of duration, disable autorotation, produce one JPEG at maximum width 480, and validate both the decoded stream and JPEG signature.

Successful generation records one weighted job-engine rate observation using source bytes. Failure and cancellation do not record throughput. Cancellation kills only the attempt's child process and removes only its known temporary file.

## Internal HTTP contract

Task 6 exposes loopback-internal routes; Task 9 will place the public authentication and secure-transport boundary around them.

- `POST /api/v1/representations`
- `GET /api/v1/representations/{representationId}`
- `GET|HEAD /api/v1/media/{representationId}/content`

Content URLs contain only opaque IDs. Delivery supports full responses, one RFC-style byte range (`start-end`, `start-`, or `-suffix`), `206`, stable `416` with `Content-Range: bytes */<length>`, `Accept-Ranges`, immutable ETags, content type/length, and `HEAD`. Multiple ranges are deliberately rejected. Tokio streams a bounded file view and never loads a whole proxy into memory.

Originals are never HTTP content sources.

## Error conditions

- `source_not_verified`: request lacks full-hash identity.
- `media_offline`: no verified original location resolves on this node.
- `derived_root_unavailable`: the configured derived root cannot accept output.
- `media_tool_failed`: FFmpeg or ffprobe could not complete.
- `invalid_media_output`: tool output failed the recipe contract.

## Remaining manual acceptance

- Real Chrome seek/playback against a generated MP4.
- Ubuntu deployment with the shared derived mount and read-only NFS/SMB originals.
- Restart/disconnect behavior on the production NAS and large 4K fixtures.
