# TV review target handoff v1 (#221)

This is the first contract slice of #220, for cloud web implementation against
synthetic data. It does not complete #221's authenticated server import/UI,
#222 recording, #224 ASR, #223 synchronization or #227 soundtrack matching.
It introduces no desktop recorder (#228).

The owning catalog exports a **path-free, immutable snapshot of derived review
evidence** after fresh verification of a completed full preview. SQLite and
`edit-plan feedback` remain canonical. The server may retain this snapshot and
review-session evidence; it must not materialize these cuts as a competing edit
plan, adopt a live SQLite/WAL file, or invent absent edit-plan history in a replica.

## Implemented local boundary

```bash
rcdvlog --catalog /absolute/project/catalog.sqlite3 edit-plan review-target-export \
  --artifact preview_id --youtube-video-id synthetic01 > handoff-envelope.json
```

`--youtube-video-id` is optional and accepts an 11-character URL-safe ID, never
an arbitrary URL. The web adapter may parse supported YouTube URL forms into
this ID; it must not fetch them. Existing success/error envelopes apply:
`{ "ok": true, "command": "edit-plan.review-target-export", "data": HANDOFF }`.
Import `data`, not the CLI envelope. Failure leaves stdout empty. This command
requires an existing explicit catalog and uses the supported writable open, which
may apply catalog migrations and acquires the existing local-heavy scheduling
lease. It makes no edit-plan/feedback write, activation, movie copy, upload,
provider call or paid analysis. For observational acceptance, use SQLite's
backup API into a separate catalog **in the same project directory** so relative
media/cache identities remain valid; never copy live SQLite/WAL files. Preserve
the owning catalog and keep that private backup out of Git.
Cancellation is supported by the Rust method; the synchronous CLI has no new
cancellation protocol.

Core owners:

- [Rust DTO, exporter, validator and picture lookup](../core/rcdvlog-core/src/editplan/review_target.rs).
- [JSON Schema](contracts/review-target-v1.schema.json) (transport shape).
- [Synthetic fixture](../core/rcdvlog-core/tests/fixtures/review-target/v1.json)
  and [fixture notes](../core/rcdvlog-core/tests/fixtures/review-target/README.md).
- [Existing bundle](edit-plan-delivery.md), [preview receipts](edit-plan-preview-artifacts.md),
  [canonical feedback](edit-plan-feedback.md), and [replica ownership](server/sync.md).

Shared typed error codes are `review_target_unsupported_version`,
`review_target_digest_mismatch`, `review_target_verification_expired`, and
`review_target_artifact_not_verified`. Invalid structural/range input uses the
existing `invalid_input` CLI code. Unknown artifacts retain the existing preview
`not_found` code. HTTP-specific authorization/project/issuer/availability states
remain the server adapter's responsibility; never parse prose to dispatch errors.

The schema version is independent of catalog, bundle and sync versions.
Unknown versions/fields are rejected. Missing optional fields deserialize as
null in Rust; producers emit all fields. JSON Schema describes this emitted
shape. Do not widen v1 to accept arbitrary JSON metadata, paths or filenames.
A new incompatible clock or mapping meaning requires a new version.

## Identity, clocks and precision

The snapshot binds project, immutable revision, `viewedArtifactId`, plan and
constraint digests, full rendered movie BLAKE3/byte length, movie clock and every
ordered cut operation. It also carries canonical declared audio, including
sound owned by a cut outside its pictured interval. Repeated asset IDs and
repeated/overlapping source ranges are legal; operation IDs must be unique.

`targetDigest` is lower-case BLAKE3 over the bytes
`vlogcutter.review-target.v1\0` followed by compact `serde_json::to_vec(target)`
from the typed Rust DTO, in declared field order. It excludes renewable
verification. **Use Rust to calculate/check this digest**, not ad hoc JavaScript
JSON canonicalization. A checksum establishes consistency, not issuer identity.
A YouTube rebind changes the snapshot/digest; retain the prior target for sessions
already bound to it. A newer active revision never changes an existing target.

`movieInPts`/`movieOutPts` are **measured output** boundaries, half-open `[in,out)`.
All integer values are within JavaScript's safe integer range. Time bases are
positive; full-picture mappings are contiguous, ordered and frame-count complete.
The rendered container may have a small audio tail beyond the last picture.
No picture candidate exists in that tail or at the final picture end.

```
elapsed movie seconds = (movie PTS - movie.originPts)
                        * timeBaseNumerator / timeBaseDenominator
```

Never interpret raw PTS as elapsed seconds; the fixture deliberately starts at
PTS 90000 with a 1/90000 time base. A point at elapsed 2 seconds selects the second
cut. A range straddling 2 seconds returns both candidates. Candidate lookup is
separate from utterance synchronization and confirmation of the intended target.

Source in/out values remain **declared source-clock locators**, not decoded
original-frame correspondence. Preview frame conversion, holds and rounding
prevent a frame-accurate source-trim claim. `declaredAudio: null` means legacy
linked-source semantics, `{"mode":"silent"}` explicitly silences that owner,
and `{"mode":"source", ...}` retains independent placement/source/gain/fades.
An audio declaration does not prove audible source presence or measured PCM
alignment. For #227, decode the final mixed audio of the exact movie digest
locally; never reconstruct its reference by concatenating picture sources.

## Verification and import responsibility

`verification` records when the owning host checked output bytes, source inputs,
constraints and renderability, with a fully decoded output receipt. These checks
establish technical evidence at that time, not chronology acceptance, editorial
quality, continuous human watching/listening, YouTube processing or TV playback.
The exporter rejects missing, replaced, modified, incomplete or stale artifacts,
and v1 rejects excerpts. Historical immutable revisions are allowed if their
exact artifact still passes; being the current active revision is unnecessary.

`validate` checks version, shape/limits, target digest, unique operations,
finite ranges, declared audio, contiguous output intervals and frame coverage.
`from_json` caps input at 16 MiB before deserialization, with at most 20,000 cuts.
`require_fresh(now, maxAgeSeconds)` adds an explicit caller-selected lifetime,
rejecting expired/future receipts. The receipt timestamp never auto-renews.
Freshness cannot establish that files still exist on an offline owning host.
Keep a separate availability observation (unknown/present/missing/modified) and
its observation time. New imports/sessions must fail preflight when required
verification or availability is unresolved; retain historical sessions/evidence.

The following is the **server integration contract**; the routes that
implement it are described under [Server import](#server-import-implemented). Claude's implementation should use existing authenticated
HTTPS/CSRF/Host/Origin guards and the generated OpenAPI client:

1. Accept metadata through a bounded explicit import/preflight, scoped to the
   authenticated project. Compare the requested project to `target.projectId`.
   An authenticated uploader is making a provenance assertion; imported JSON
   booleans do not independently verify media or authenticate a catalog issuer.
   Record importer identity, owning-host provenance and import/observation times
   separately. Never label an unchecked bundle `serverVerified`.
2. Run the shared Rust validator and explicit freshness/availability policy.
   Distinguish unsupported version, malformed/digest mismatch, project/revision
   mismatch, incomplete mapping, stale verification, unavailable artifact and
   unverifiable issuer. Do not silently replace a target or fall back to current
   catalog revision when any check fails.
3. Deduplicate by project + `targetDigest`; identical imports reuse the target.
   A refreshed verification may append an observation to that target. Reuse of
   `(projectId, viewedArtifactId)` with different movie bytes, plan, revision or
   cut mapping is a conflict, not an overwrite. A changed YouTube association
   creates a new target snapshot while preserving prior session identity.
4. Allocate an opaque server review-target ID and return an immutable snapshot
   for every recording session. Display revision, digest, duration, mapping
   precision, verification time/status and **YouTube identity: declared/unverified**.
   Link absence is valid. Never claim a pasted URL proves the TV's bytes.
5. Export review notes for explicit reconciliation on the owning catalog through
   the supported commands below. Do not add a second feedback write owner.

No raw bundle upload is needed: existing `provenance.json`, context files,
coverage and source inspection can contain private metadata. Only this allowlist
snapshot crosses the handoff boundary, privately and on explicit import. No
movie, PCM, transcript, source clock metadata, filename, absolute path, tool path,
free-text rationale or private real-project identifiers belong in cloud fixtures.

## Server import (implemented)

The companion server owner is
[`review_targets.rs`](../core/rcdvlog-server/src/review_targets.rs) (control
catalog migration 13); the HTTP adapter is
[`review_target_http.rs`](../apps/server/src/review_target_http.rs). All routes
sit behind the existing session/device authentication, CSRF on mutations, and
Host/Origin/forwarded-header validation, with a request body limit sized to the
validator's 16 MiB cap.

| Route | Purpose |
| --- | --- |
| `POST /api/v1/projects/{projectId}/review-targets/preflight` | Non-mutating checks: version, shared Rust validation and digest, verification receipt, project, freshness, availability, identity/conflict. Content failures are `fail` checks with stable codes, not HTTP errors. |
| `POST /api/v1/projects/{projectId}/review-targets/import` | Same checks inside one immediate transaction. `created`, `reused` (identical digest and receipt) or `refreshed` (identical digest, new receipt appended as an observation). |
| `GET /api/v1/projects/{projectId}/review-targets` | Bounded newest-first list (`limit` 1–200, `before` cursor). |
| `GET /api/v1/projects/{projectId}/review-targets/{reviewTargetId}` | Immutable snapshot, per-cut elapsed movie seconds, mapping precision, provenance, every verification observation, session readiness and fixed limitations. |
| `POST /api/v1/projects/{projectId}/review-targets/{reviewTargetId}/youtube` | Declare or clear an 11-character ID. Creates (or reuses) a separate snapshot digested by Rust, records `derivedFromReviewTargetId`, and leaves the original untouched. |
| `GET /api/v1/projects/{projectId}/review-targets/{reviewTargetId}/candidates` | `picture_candidates` for an elapsed movie point/range. Picture locators only. |

Request body: `{ "handoff": <data>, "declaredIssuer": "Travel Mac"?, "availability": {"state","observedAt"}?, "maxVerificationAgeSeconds": n? }`.

Policy decisions, recorded here so later owners do not re-derive them:

- **Freshness.** New imports and new sessions require a receipt at most
  `REVIEW_TARGET_SERVER_MAX_VERIFICATION_AGE_SECONDS` (7 days) old. A caller may
  request a stricter lifetime, never a longer one. The accepted lifetime is
  stored with each observation, and readiness uses the **strictest lifetime
  ever accepted** for the target. A YouTube rebind inherits it. An imported
  target that later goes stale keeps all evidence; its readiness reports
  `review_target_verification_expired` as a blocker.
- **Availability.** Absent an explicit assertion, the server records `present`
  at `verifiedAt` with source `exportVerification` (the exporter refuses
  missing/modified bytes). `unknown`, `missing` or `modified` blocks import
  with `review_target_artifact_unavailable`. If the same target was already
  imported, that adverse report is still **retained as an observation**. The
  most recently observed availability governs readiness, so the target stops
  being ready for new sessions until a later `present` observation.
- **Identity.** Dedup is `(projectId, targetDigest)`. A *binding digest* — the
  same Rust digest with the YouTube ID cleared — detects reuse of
  `(projectId, viewedArtifactId)` with different movie bytes, plan, revision or
  cut mapping, which is `409 review_target_conflict`; nothing is overwritten.
  A different YouTube ID with the same binding is a new target, not a conflict.
- **Provenance.** Importer actor (browser session or device credential),
  import time and an optional declared issuer label (≤64 characters, no
  paths) are stored separately. `issuerVerification` is always `unverified`
  and `youtube.identityStatus` is `declaredUnverified` or `absent`.
- **Revision.** The server replica has no edit-plan history, so a revision is
  never checked against or replaced by the "current" revision; a revision
  mismatch for the same viewed artifact is a conflict.

Additional stable codes: `review_target_invalid` (malformed/unknown field/
incomplete or overlapping mapping), `review_target_project_mismatch`,
`review_target_artifact_unavailable`, `review_target_conflict`, plus
`project_replica_not_found`. The four exporter codes above pass through
unchanged. The web *TV review* screen accepts the bare handoff or the CLI
success envelope (it sends only `data`), parses YouTube watch/youtu.be/shorts/
live/embed links into an ID without fetching them, and branches on codes only.

Tests: core unit tests in `review_targets_tests.rs` (dedup, refresh, every
rejection code, conflict per field, rebind, staleness, candidate lookup with
the PTS-origin offset and audio tail, pagination); an HTTP route test for
auth/CSRF/409/400/audit; Vitest component tests; and a Chrome Playwright test
against a real disposable server seeded with a Rust-digested synthetic handoff.
Not exercised: real exported handoffs, HTTPS behind Caddy/Tailscale, or any
YouTube/TV playback.

Recording sessions bound to an imported target are described in
[TV voice review recording](tv-voice-review-recording.md) (#222).

## Canonical feedback reconciliation

For a reviewed and corrected note, send the existing `RecordReviewNoteRequest`
through `edit-plan feedback record` on the owning catalog. Carry exact project,
revision, operation, `viewedArtifactId`, optional `movieTimeSeconds` and opaque
session/utterance evidence references. Author `reviewer` is available for machine
observations; only an explicit human action may assert human authorship/decision.
Do not automatically convert TV dialogue, a transcript, a sync candidate, a
proposal or provisional agent choice into a human acceptance.

A movie locator alone cannot choose a cut for a wide/uncertain utterance. Keep
recording-time evidence and alternatives until the reviewer corrects/confirms
its target. The existing feedback command records a **declared** playback locator;
it does not validate sync or derive source trims. Reconcile each note with a
persistent session-note → canonical note-ID receipt to prevent retry duplicates;
the existing `record` command has no idempotency-key parameter. Do not blindly
replay it after a lost response. `propose`, `decide`, explicit `map`, and guarded
`apply` retain their existing revisions, provenance and acceptance checks.

## Validation split and next work

Cloud CI uses only the synthetic fixture. It includes split/repeated sources,
nonzero PTS origin, a container audio tail, explicit silence and independently
timed sound. Mutations in Rust tests cover digest mismatch, version/field/size
rejection, duplicates, gaps, overlaps, bad clocks, stale verification and unsafe
IDs. A generated-media test renders/decode-verifies a movie, exports twice and
refuses changed or missing bytes. A CLI process contract tests envelopes and
preservation of immutable revisions.

Real-project validation receipts and any audio stay outside Git. Record exact
candidate commit, executable digest, revision/artifact/mapping/movie identities,
commands, tool versions, measured errors, coverage and failures. Compare #223
anchors to independent known times; label injected/cropped-reference tests
separately from room/TV/microphone tests. Report unknowns and false matches.
Synthetic mapping tests cannot establish browser capture, speaker ASR, real TV
playback, acoustic delay or YouTube timing. No cloud PR existed at this handoff's
initial inspection; validate each later candidate in an isolated worktree.

Priority remains #221 import integration, then the shared #223 anchor mapper,
then #227 final-mix matching. Do not duplicate the cloud web recorder or ASR.
