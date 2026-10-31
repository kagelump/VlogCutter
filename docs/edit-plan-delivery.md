# Local edit-plan review bundles

`edit-plan bundle --input bundle.json` creates a portable, local review folder
from one completed preview artifact. The core verifies the exact artifact and
its current source/constraint identity before copying it. It accepts either a
complete revision preview or a bounded cut neighborhood. It does not render,
activate, change a revision, invoke a provider, or upload media.

The request is capped by the CLI adapter and has this shape:

```json
{
  "artifactId": "preview-artifact-id",
  "outputDirectory": "/chosen/review-folder",
  "name": "antarctica-rough-cut-r1",
  "contactSheet": false,
  "contextEvidenceId": null,
  "pictureCoverage": null
}
```

`outputDirectory` is the caller-selected parent directory. `name` is a stable
portable folder component containing only ASCII letters, digits, `_` and `-`.
The resulting folder contains `bundle-owner.json`, `movie.mp4`, `mapping.json`,
`provenance.json`, `coverage.json`, `index.html`, and `manifest.json`. With `contactSheet: true`,
bounded neighborhoods also contain three rendered frames per cut (start,
middle, and end), up to 36, plus `contact-sheet.json`. Full-revision contact
sheets are omitted because they have no bounded frame budget. Supplying a
matching `contextEvidenceId` embeds the existing context evidence in the review
index and packages its receipt-verified source frame images and playable audio
files beside the movie; it adds no new inspection or provider work. Optional `pictureCoverage`
embeds an inventory-based picture coverage report, and its revision must match
the verified artifact. The caller may provide a canonical `inventoryAssetIds`
scope when the project inventory contains non-footage placeholders.

The copy is a full-byte copy whose BLAKE3 digest and length must match the
verified preview receipt. A create-only ownership marker binds the folder to
the artifact, output name, and request before any media is copied. Every file is
create-only, and `manifest.json` is published last, after the other files have
been synced. A later call can reuse
an existing bundle only when the manifest names the same artifact and all
listed file hashes and lengths still match. Existing unrelated folders, files,
and damaged bundles are never replaced. If publication stops before the
manifest, a later call takes the released directory lock, checks the ownership
marker, rejects unrelated contents, and reuses only files whose bytes match the
current verified receipts or deterministic bundle documents. The absent
manifest marks the folder incomplete until publication finishes.

`mapping.json` distinguishes the encoded movie clock from source clocks. Movie
cut positions are derived from the verified output PTS and its declared time
base/origin. Source in/out values and source stream clocks remain declared
locators; preview resampling and frame holds do not establish exact source
frame identity. Attached context evidence also contributes its measured 48 kHz
audio mappings. Canonical selection metadata supplies chapter spans when
available; the bundle does not invent chapter boundaries.

The standalone `index.html` plays the copied rendered movie, lists cut IDs,
source asset handles, source ranges, and movie ranges, and optionally includes
the already-recorded context evidence. The movie's actual audio is preserved
and playable. `coverage.json` carries the local review coverage report and its
limitations. Coverage and technical verification are evidence, not human
approval, word-completeness, continuity, or pacing certification.
