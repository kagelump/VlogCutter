# T01-T004: Low-Bandwidth Mobile Proxy Recipe

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "T01-T004",
  "epic_id": "T01",
  "title": "Low-Bandwidth Mobile Proxy Recipe",
  "slug": "mobile-proxy-recipe",
  "status": "ready",
  "task_class": "implementation",
  "depends_on": [],
  "principal_invariant": "A new mobile proxy representation kind generates correctly oriented, seekable output that is 10–20x smaller than the 720p proxy, matching the existing proxy's orientation behavior on all real-camera fixtures.",
  "central_falsification": "Generate a mobile proxy from each real-camera fixture (Sony, DJI, iPhone, Insta360), assert ffprobe orientation matches the existing proxy recipe's output exactly, and assert the mobile proxy file size is at most 10% of the 720p proxy size on the same source.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Representation recipe → durable job engine (S01-T004)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "implementer": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/REPRESENTATIONS_AND_STREAMING.md"
    ],
    "independent_tester": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/travel/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/travel/tasks/T01-T004-mobile-proxy-recipe.md"
    ],
    "implementer": [
      "core/rcdvlog-server/src/representations.rs",
      "apps/server/src/lib.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-server/tests/**"
    ],
    "integrator": [
      "packages/project-client/**"
    ]
  },
  "forbidden_paths": [
    "core/rcdvlog-core/src/catalog/**",
    "apps/desktop/**",
    "apps/web/**",
    "apps/mobile/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-server",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/travel/EPIC.md",
      "regex": "\\| 40 \\| \\[T01-T004\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Add a third representation kind — a small, seekable, correctly oriented mobile
proxy — so that arriving footage can be reviewed on a phone over hotel bandwidth
without competing with the uploads still in flight.

## Principal invariant

This task is correct when a new mobile proxy representation kind generates
correctly oriented, seekable output that is 10–20x smaller than the 720p proxy,
matching the existing proxy's orientation behavior on all real-camera fixtures.

## Inputs and outputs

Inputs:

- S01-T006 representation store and existing recipe infrastructure
- S01-T004 durable job engine (FFmpeg worker)
- Existing real-camera orientation fixture set

Outputs:

- `RepresentationKind::MobileProxy` with recipe key `mobile.h264-480p.orientation-v1`
- FFmpeg settings: H.264, `scale=min(854,iw):-2`, CRF 30, `veryfast`, `yuv420p`, AAC 64k mono, MP4 `faststart`
- Output validation before commit: probe-able, non-zero duration, expected dimensions, correct orientation
- Byte-range streaming through existing representation media endpoints

## Frozen decisions

1. The recipe is immutable and versioned; changing settings requires a new key.
2. Representation identity remains verified input plus versioned recipe.
3. Output is validated before commit and committed atomically.
4. Mobile proxies are enqueued by the travel ingest path only. No library-wide sweep exists.
5. Generation shares the existing one-worker FFmpeg bound; no parallel worker added.
6. A mobile proxy is disposable; losing one is a regeneration, never a data loss.
7. Existing proxy and thumbnail behavior is unchanged. No existing recipe key or output is altered.
8. Range requests work in mobile Safari and React Native without downloading the whole file.
9. Orientation must match the existing proxy's behavior — reproduce `display_rotation=0` and `noautorotate`, not merely copy the settings string.

## Owned paths

- **Implementer:** `core/rcdvlog-server/src/representations.rs` (kind, recipe key, settings, extension, content type), `apps/server/src/lib.rs` (representation request and media routes)
- **Independent tester:** `core/rcdvlog-server/tests/` with real-camera orientation fixtures
- **Integrator:** `packages/project-client/` (representation kind in typed contracts)

## Required interface

- `RepresentationKind::MobileProxy` variant in shared DTOs
- Recipe settings identity for the representation store
- Mobile proxy enqueue from the travel ingest path (T01-T003)

## Acceptance oracle

1. `mobile.h264-480p.orientation-v1` generates through the existing durable job path.
2. Output is atomically committed and identified by verified input plus recipe.
3. Output is materially smaller than the 720p proxy on representative media.
4. Orientation matches the existing proxy's behavior on Sony, DJI, iPhone, and Insta360 fixtures.
5. Rotated-metadata source media produces upright output at 854-wide, not only at 1280-wide.
6. Invalid or zero-duration output is rejected and never committed.
7. Range requests permit seeking without a full download.
8. Existing proxy and thumbnail recipe keys and outputs are unchanged.
9. No library-wide mobile proxy sweep exists or can be triggered accidentally.
10. FFmpeg concurrency bounds are unchanged.
11. Regenerating a deleted mobile proxy produces an identical result.

## Independent test charter

Try to falsify: "the mobile proxy orientation matches the existing proxy." Use
the real-camera fixture set, not only synthetic MPEG-4. Assert orientation
through `ffprobe` output rather than by eye. Include at least one clip whose
rotation is expressed as a container display matrix rather than a stream-level
tag. Measure output size against the 720p proxy on the same sources and record
the ratio. Verify that no library-wide sweep trigger exists. Verify that
existing recipe keys are unmodified.

## Review charter

Risk domain: orientation correctness and recipe immutability. Questions: (1) Does
the mobile proxy produce upright output from rotated-metadata sources? (2) Is
output validated before commit? (3) Is the existing proxy recipe unmodified? (4)
Can a library-wide sweep be triggered? Severity: P0 for silently sideways output
on real camera media; P1 for missing output validation or existing recipe
modification. Approval vocabulary: `APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Library-wide backfill of mobile proxies across the NAS archive
- Hardware transcoding
- Adaptive bitrate or multiple mobile quality tiers
- Audio-only or thumbnail-strip representations
- Client-side caching policy (T01-T008)

## Stop conditions

Stop and escalate when:

- Orientation behavior diverges from the existing proxy on any real-camera fixture
- The mobile recipe requires a parallel FFmpeg worker
- Output validation cannot distinguish valid from invalid mobile proxy output
- The recipe needs per-camera-model tuning rather than one settings key

## Commands

```sh
cargo test -p rcdvlog-server
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

T01-T008 caches these on the phone under a size budget. Keep the content type and
range behavior identical to the existing proxy so the phone player needs no
special case.
