# Orientation review

The catalog keeps camera metadata rotation, the landscape-first inferred default,
the saved effective rotation, and an optional human decision as separate facts.
Only `orientationDecisionSet` records visual confirmation. Setting a different
rotation is an explicit override but does not confirm the image. Confirming the
default is still an explicit decision.

Use `clips orientation-queue --project PROJECT --limit 50` to read up to 200
suspicious, unresolved, or changed-confirmation assets. Use
`clips orientation-evidence --asset ASSET` to generate two bounded PNG stills
under the catalog's managed `cache/orientation` directory. It uses an available
proxy when the original is offline and reports which representation supplied
the frames. The raw-metadata still follows the selected representation's raw
orientation metadata; the effective still uses the saved rotation. The files
are projections and can be regenerated; the operation log remains canonical.

Confirm one clip with `clips orientation-confirm --asset ASSET
--expected-revision REVISION --rotation-degrees 90`. Explicitly defer with
`clips orientation-unresolved --asset ASSET --expected-revision REVISION`.
Batch review accepts repeated `--confirm ASSET:REVISION:DEGREES` and
`--unresolved ASSET:REVISION` values. The whole batch is rejected before any
decision is written if an asset revision is stale or the batch spans projects.
An unresolved decision leaves the saved rotation unchanged. A later rotation
change remains visible as a confirmed-value mismatch until reviewed again.

Metadata refresh updates raw metadata and inferred defaults while preserving
operation-backed rotation decisions. It never infers image orientation from
map or globe layout, and this workflow makes no visual-model requests.
