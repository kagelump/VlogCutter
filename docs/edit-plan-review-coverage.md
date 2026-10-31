# Audiovisual review coverage

Schema 64 stores review coverage independently from edit-plan feedback. A
coverage record identifies the project, immutable revision, stable cut ID,
verified preview artifact, actor, method, record time, evidence references,
uncertainty, and one or more intervals on the artifact's movie clock. Every
interval names a modality (`picture` or `listening`) and whether it is
`continuous` or `sampled`. The ledger is append-only evidence; changing a plan
or reviewing a different artifact creates additional records.

An explicit playback caller may attest to human review only with
`actor: human`, `method: humanPlayback`, and
`explicitHumanPlayback: true`. The catalog requires a completed preview whose
output, inputs, constraints, and renderability pass fresh verification before
it accepts that attestation. `agentReview`, `modelReview`, `sparseSampling`,
`transcriptInspection`, `decodeCheck`, and `energyCheck` can be recorded as
other evidence, but cannot count as human picture or listening coverage. The
explicit flag is an assertion by the caller; the catalog cannot observe a
person watching or listening.

Intervals are bounded by the verified movie duration and must overlap the
artifact mapping for the named cut. An excerpt-local movie locator is rebased
onto its full revision clock origin before it enters the report or next-review
queue. Without a full preview mapping, times use the planned 30000/1001 frame
clock and the report labels that as an estimate. Reports compute per-cut picture and
listening coverage independently. Only continuous human playback intervals
close a gap. Sampled intervals are unioned and shown as sampled seconds, while
the remaining gaps become a bounded next-review queue (at most 100 items).
Repeated or overlapping intervals do not inflate covered duration.

Coverage remains attached to its original revision and artifact. Reports may
carry an interval to a later revision only when the stable cut and its previous
and next cut snapshots, source ranges, source reuse, contextual audio digest,
source fingerprint, orientation, and selected representation fingerprint all
match. The interval is stored relative to its mapped cut boundary, so a prefix
addition can shift its movie location without changing the reviewed cut.
Changed dependencies leave the old record historical and open a new gap. The
`verify` report option rechecks referenced old and current artifacts and
excludes evidence whose artifact is no longer current; without it the report is
a historical ledger view and does not certify current media bytes. A report
examines at most 1,000 recent historical records and marks truncation.

The report is not a claim about comprehension, complete dialogue, source-frame
identity, editorial acceptance, pacing quality, or meaningful listening. The
movie intervals locate a human playback assertion in a rendered preview, whose
source-frame sampling may include resampling and holds. Feedback notes,
proposals, decisions, source inspections, and plan activation keep their
existing separate owners.

CLI adapters should expose bounded JSON requests and paginated history, using
the core methods `Catalog::record_review_coverage`,
`Catalog::review_coverage_report`, and `Catalog::review_coverage_records`.
