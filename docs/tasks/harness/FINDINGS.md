# H01 findings

Production observations made while writing the fast CUJ suite. None were fixed
in that epic: its non-goals put behavior changes outside its scope, so each was
recorded with the test that pinned the behavior. A test that pins a defect names
it, so the fix flips a red test rather than silently changing an untested one.

Findings 1, 4, 5, and 6 have since been resolved by #18/#28 (the project-session
owner and its stale-read guard) and #19/#29 (one mutation lifecycle and an
explicit unsaved-draft state). Each is marked below with what it became; the
observation is kept because the tests that pinned it are now the tests that
prove the fix.

Nothing in this file is a chronology, media, or native claim; see
`docs/tasks/harness/ARCHITECTURE.md` §9 for what this tier may and may not be
cited for.

## 1. Six API adapters have no callers — RESOLVED (#19)

`api.createNoteRevision`, `api.addLabel`, `api.removeLabel`, `api.setRemoved`,
`api.setRotation`, `api.generateThumbnail`, and `api.ingestFolder` are exported
and reachable, and nothing in the desktop frontend calls any of them.

Every review judgment goes through `persistAsset`, which sends the whole
`AssetPatch` via `update_asset`. So the narrower commands — including
`create_note_revision`, which exists precisely so a note becomes a revision
rather than a field overwrite — are only exercised by the CLI and by Rust tests.

Decided: the adapters were deleted. `session/assetMutations.ts` owns the single
write path, and one path is what made the failure policy stateable at all;
routing notes through `create_note_revision` would change what a note *is* and
is a product decision rather than a refactor. The Rust commands stay registered
for the CLI, and `harness/contract.ts` now carries the decision beside each
uncalled command so the gap cannot drift back into an accident.

Pinned by: `review.cuj.test.tsx` ("sends the whole patch, because rotation is
part of the asset contract"), which asserts the `update_asset` payload.

## 2. The desktop records where you were and never takes you back

`record_desktop_context` fires on every selection change, and
`DesktopSettings.lastAssetId` is faithfully stored. On launch, `App` restores
the project and then selects `result[0]` — the first clip in the queue. The
recorded clip is never read.

Pinned by: `review.cuj.test.tsx` (CUJ 12), which asserts judgments are restored
and the selection lands on the first clip.

## 3. `generate_proxy` is unreachable through the interface

The single-clip "Make proxy" button lives on the stage's offline slate, which
renders only when the selected clip has **no** playable URL. A clip with no URL
has no original either — and `generate_proxy` needs the original. The only state
that shows the button and could succeed is "the loopback media origin failed to
bind", where making a proxy does not help.

The batch (`generate_missing_proxies`) covers the real need, so this is a dead
affordance rather than a missing capability.

Pinned by: `generation.cuj.test.tsx`, which exercises `generate_proxy` at the
command boundary and says why.

## 4. A finished analysis run does not refresh the clip you are looking at — RESOLVED (#18)

`runAnalysis` refreshes assets, the analysis index, and readiness. The per-clip
transcript and observation read is keyed on `selectedId`, so the evidence rail
for the clip already on screen stays empty until the selection moves. The queue
row's verdict updates; the rail beside it does not.

Pinned by: `analysis.cuj.test.tsx` ("reports each stage as it happens…"), which
now asserts the opposite: a finished run calls `session.reload()`, which
re-reads the selected clip's evidence along with the queue, so the rail fills
without a selection change.

## 5. A failed project load has no way back in — RESOLVED (#18)

`refreshAssets` runs when the open project changes. If that read fails — a
locked catalog, a busy volume — the error is shown and the queue stays empty,
and there is no "try again" anywhere in the interface. Switching modes does not
re-read. Relaunching the app does.

Pinned by: `recovery.cuj.test.tsx` ("a project that will not open"), which now
asserts the recovery: the queue offers *Try again*, and a reload control in the
queue heading also lets the app notice a backend change without a write. That
control is what let the CUJs stop clicking *rotate* to force a read.

## 6. A failed save keeps the draft, but nothing retries it — RESOLVED (#19, #29)

Better than expected, and worth stating because the opposite is the usual bug:
`persistAsset` rolls back to the asset it was given, which already carries the
user's typed text, so a rejected note save leaves the paragraph on screen. There
is no automatic retry — the save is driven by leaving the field — so the user
must touch the field again for it to land. That is defensible; it is also
invisible, since the error message does not say so.

Now stated rather than accidental: `AssetMutationLifecycle` carries a failure
policy per mutation — `rollback` for labels, removal, and rotation;
`retain-draft` for notes — and a retained draft is marked *Unsaved* with a
*Retry* that sends the same text. Drafts are written back over any subsequent
read, so a reload cannot silently replace a paragraph with the value the
catalog still holds.

Pinned by: `recovery.cuj.test.tsx` ("keeps the draft on screen, marks it
unsaved, and retries it unchanged") and the unit tests in
`src/session/assetMutations.test.ts`, which ask what survives a rejection with
nothing rendered.

## 7. One pre-existing flaky test

`components/__tests__/judgmentRail.test.tsx` — "does not read the observation
again on every keystroke" — installs its spy after `renderApp` resolves, and a
selection effect can still be in flight. It failed once under the added parallel
load of the new suite and has passed every run since. The fix is to assert on
calls made after a settled point rather than on a zero count.

## 8. The offline slate blames a missing proxy for a missing origin

The stage renders its slate whenever the selected clip has no playable URL, and
the slate's sentence is chosen from the clip: "Proxy is missing" when a proxy
path is recorded but unavailable, otherwise "No review proxy yet". It never
considers the third reason a URL can be absent — that the app has no media
origin to build one from.

So a clip with a perfectly good proxy is told it has none. On the desktop that
is what the user sees when the loopback origin fails to bind; in the browser
preview, which has no origin by design, it is what every clip says. The
adjacent "Make proxy" button then offers a repair that cannot help, which is
finding 3 arriving from the other direction.

Found by: the H01-T010 visual pass over the preview. Pinned by:
`mediaUrl.test.ts` ("has no URL to give until the origin has been handed over"),
which fixes the adapter's half of the behaviour; the slate's wording is
untested and unchanged.
