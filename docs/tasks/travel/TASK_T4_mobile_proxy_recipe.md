# Task T4: Low-Bandwidth Mobile Proxy Recipe

**Status:** Planned
**Depends on:** Companion-server Task 6
**Unblocks:** T8

## Objective

Add a third representation kind — a small, seekable, correctly oriented mobile proxy — so
that arriving footage can be reviewed on a phone over hotel bandwidth without competing
with the uploads still in flight.

## User outcome

The user can watch and scrub a clip on their phone minutes after it arrives, and can keep
reviewing it offline afterward, without waiting on a 720p download.

## Scope

- New `RepresentationKind::MobileProxy` with recipe key `mobile.h264-480p.orientation-v1`.
- FFmpeg settings string and deterministic settings identity.
- Extension, content type, and derived-media path participation.
- Generation through the existing durable representation job path.
- Byte-range streaming through the existing representation media endpoints.
- Enqueue only for freshly archived travel media.

## Out of scope

- Library-wide backfill of mobile proxies across the NAS archive
- Hardware transcoding
- Adaptive bitrate or multiple mobile quality tiers
- Audio-only or thumbnail-strip representations
- Client-side caching policy (T8)

## Recipe

| Property | Value |
| --- | --- |
| Recipe key | `mobile.h264-480p.orientation-v1` |
| Video | H.264, `scale=min(854,iw):-2`, CRF 30, `veryfast`, `yuv420p` |
| Audio | AAC 64k mono |
| Container | MP4 with `faststart` |
| Metadata | stripped, as with the existing recipes |
| Target size | roughly 10–20x smaller than `proxy.h264-720p.orientation-v3` |

Exact settings are the implementer's to tune against real media, but the recipe key must be
bumped whenever settings change, exactly as the existing recipes do.

## Orientation is the risk

The existing proxy recipe key carries an `orientation-v3` suffix because orientation
handling has been revised repeatedly, and the settings string encodes `display_rotation=0`
and `noautorotate` to make the result deterministic.

The mobile recipe must **reproduce that behavior**, not merely copy the settings string.
Camera rotation metadata, container-level display matrices, and per-vendor quirks interact
with scaling, and a recipe that is correct for 1280-wide output is not automatically
correct for 854-wide output.

Validate against the same real-camera fixtures used for the existing proxy — Sony, DJI,
iPhone, and Insta360 — and assert the output's `ffprobe` orientation contract, not just
that a file was produced.

## Design requirements

1. The recipe is immutable and versioned; changing settings requires a new key.
2. Representation identity remains verified input plus versioned recipe.
3. Output is validated before commit — probe-able, non-zero duration, expected dimensions,
   correct orientation — and committed atomically, as with existing recipes.
4. Mobile proxies are enqueued by the travel ingest path only. No library-wide sweep exists.
5. Generation shares the existing one-worker FFmpeg bound; it does not add a parallel worker.
6. A mobile proxy is disposable. Losing one is a regeneration, never a data loss.
7. Existing proxy and thumbnail behavior is unchanged. Adding a variant must not alter
   either existing recipe key or its outputs.
8. Range requests work in mobile Safari and in the React Native player without downloading
   the whole file first.

## Likely implementation areas

- `core/rcdvlog-server/src/representations.rs` kind, recipe key, settings, extension, content type
- `apps/server/src/lib.rs` representation request and media routes
- `packages/project-client` representation kind in typed contracts
- Real-camera orientation fixtures and regression tests

## Acceptance criteria

- [ ] `mobile.h264-480p.orientation-v1` generates through the existing durable job path.
- [ ] Output is atomically committed and identified by verified input plus recipe.
- [ ] Output is materially smaller than the 720p proxy on representative media.
- [ ] Orientation matches the existing proxy's behavior on Sony, DJI, iPhone, and Insta360 fixtures.
- [ ] Rotated-metadata source media produces upright output at 854-wide, not only at 1280-wide.
- [ ] Invalid or zero-duration output is rejected and never committed.
- [ ] Range requests permit seeking without a full download.
- [ ] Existing proxy and thumbnail recipe keys and outputs are unchanged.
- [ ] No library-wide mobile proxy sweep exists or can be triggered accidentally.
- [ ] FFmpeg concurrency bounds are unchanged.
- [ ] Regenerating a deleted mobile proxy produces an identical result.

## Verification

Use the real-camera fixture set, not only synthetic MPEG-4. Assert orientation through
`ffprobe` output rather than by eye, and include at least one clip whose rotation is
expressed as a container display matrix rather than a stream-level tag. Measure output size
against the 720p proxy on the same sources and record the ratio.

## Handoff notes

T8 caches these on the phone under a size budget. Keep the content type and range behavior
identical to the existing proxy so the phone player needs no special case.
