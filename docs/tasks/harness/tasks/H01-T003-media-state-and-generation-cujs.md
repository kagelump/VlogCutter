# H01-T003: Media state and generation CUJs

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "H01-T003",
  "epic_id": "H01",
  "title": "Media state and generation CUJs",
  "slug": "media-state-and-generation-cujs",
  "status": "draft",
  "task_class": "implementation",
  "depends_on": ["H01-T001"],
  "principal_invariant": "Proxy and thumbnail generation change per-asset availability in canonical state as each item finishes, reported counts equal the changes made, and media availability transitions are visible to the review surface without a reload.",
  "central_falsification": "Report generation that changes no asset — the current browser-preview behavior — and prove a named CUJ fails on the asset read-back.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "generation commands -> per-asset availability in canonical state"
  ],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/harness/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/harness/ARCHITECTURE.md", "docs/tasks/desktop/MEDIA_DELIVERY.md", "docs/proxy-generation.md"],
    "independent_tester": ["docs/tasks/desktop/MEDIA_DELIVERY.md", "docs/integration-testing.md"],
    "reviewer": ["docs/tasks/harness/ARCHITECTURE.md", "AGENTS.md"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/harness/tasks/H01-T003-media-state-and-generation-cujs.md"],
    "implementer": ["apps/desktop/src/test/harness/commands/media.ts", "apps/desktop/src/test/harness/media.ts", "apps/desktop/src/test/cuj/generation.cuj.test.tsx", "apps/desktop/src/test/cuj/playback.cuj.test.tsx"],
    "independent_tester": ["apps/desktop/src/__independent__/media-availability.independent.test.tsx"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "forbidden_paths": ["core/**", "apps/desktop/src-tauri/**", "apps/web/**", "apps/server/**", "apps/cli/**"],
  "verification_commands": [
    "pnpm --dir apps/desktop test",
    "pnpm --dir apps/desktop check"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/harness/EPIC.md",
      "regex": "\\| \\[H01-T003\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Make media generation and media availability behave like a backend, and give
CUJs a way to drive the mounted `<video>` element deterministically. CUJ 8 and 9
from the design brief, plus the playback orchestration a review session depends
on.

## Principal invariant

`generate_proxy`, `generate_missing_proxies`, `generate_thumbnail`, and
`generate_missing_thumbnails` mutate the assets they claim to have processed;
their reported counts equal the number of assets whose state changed; and every
progress event the UI renders was emitted by the test.

## Inputs and outputs

Inputs: harness from H01-T001; the generation command family; `proxy-progress`
and `thumbnail-progress` events; `mediaUrl`/`thumbnailUrl` construction;
`recover_media_origin`; `detect_offline_volumes`; `relink_folder`.

Outputs: a generation command module whose handlers mutate state per item, a
`harness.media(assetId)` helper that dispatches real DOM media events at the
mounted element, and CUJs for the generation and offline-recovery journeys.

## Frozen decisions

- Generation is scripted: the command promise stays pending until the test
  completes or fails the job, and each `progress` step marks one asset done.
- `harness.media(id)` dispatches genuine `loadedmetadata`, `playing`, `waiting`,
  `seeked`, `timeupdate`, and `error` events at the element the app mounted. It
  never stubs `HTMLMediaElement` methods beyond what jsdom forces (`play`,
  `pause`, `load`).
- `mediaUrl` runs for real: the harness supplies a `mediaOrigin` in
  `desktop_settings`, so representation choice, token, and id encoding are
  asserted from the produced URL.
- Nothing in this task is evidence for decoding, first-frame paint, or
  unbuffered seeking. Those stay in the native tier per
  `docs/tasks/desktop/MEDIA_DELIVERY.md`.
- Offline transitions are asset-level state (`originalAvailable`,
  `proxyAvailable`, `available`), and the UI must learn about them through a
  read the app already performs.

## Owned paths

The implementer owns the media command module, the media-event helper, and the
two CUJ files. The independent tester owns one file under `__independent__/`.

## Required interface

```ts
const job = harness.job("generate_missing_proxies");
await job.progress({ assetId: "asset_sony_003" });   // marks that asset done
await job.complete();                                 // resolves the command

harness.media("asset_sony_001").loaded({ duration: 12.4 });
harness.media("asset_sony_001").playing();
harness.media("asset_sony_001").failed("decode");
harness.asset("asset_sony_003").setOriginalAvailable(false);
```

## Acceptance oracle

1. A project with clips lacking proxies shows the generate action; running it
   emits progress the TopBar renders, and each completed item flips that asset's
   `proxyAvailable` in state.
2. The reported `generated` count equals the number of assets changed, and
   `skipped` equals those already having proxies.
3. After generation, the selected clip's media URL uses the proxy
   representation; before it, it uses the original.
4. Thumbnail generation sets `thumbnailPath` and `thumbnailQuality` per asset.
5. Play, pause, buffering, and position UI follow the synthetic media events, and
   the keyboard transport contract still holds.
6. A media `error` triggers the app's recovery call, and a recovered origin
   restores playability without losing the selection.
7. Marking a source offline mid-review surfaces the condition on the next read
   the app performs; restoring it and refreshing clears the condition.
8. A generation failure leaves already-finished items changed and the failed item
   unchanged.

## Independent test charter

Attack the accounting: generate with a mixture of present, missing, and stale
proxies and check counts against state diffs; cancel midway and check the
finished prefix persisted; take a volume offline while a job is running. Assert
nothing about visual media.

## Review charter

Risk domain: a fake that makes media look better than it is. Ask: does any
handler report a count it did not effect? Does any test assert playback
correctness rather than orchestration? Does any assertion imply frame
production? Severity: a claim about native media behavior is blocking.

## Non-goals

- Real decoding, seeking, or WKWebView behavior.
- FFmpeg recipe correctness.
- Import journeys (H01-T007).

## Stop conditions

Stop and escalate when jsdom cannot deliver an event the app depends on, or when
availability cannot be observed through any existing frontend read.

## Commands

```sh
pnpm --dir apps/desktop test
pnpm --dir apps/desktop check
```

## Handoff

Report handlers added, media events supported, any jsdom limitation worked
around and how, and every assertion deliberately left to the native tier.
