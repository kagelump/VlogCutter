# Native video review CLI

The `native-video` command family is a thin JSON adapter over the core bounded native-video workflow. It accepts a complete typed `RunPolicyRequest` JSON document; its `parameters` must have kind `native_video_review`. Keep provider/model fixed to the registered `gemini-direct` / `gemini-3.5-flash-lite` pair. Policy inputs are limited to 8 MiB and window lists to 64 KiB, read through a capped stream (including FIFOs).

An options document uses schema version 1 and explicit purpose (`moments` or `endpointReview`), tier (`standard` or `flex`), representation (`original`, `generatedProxy`, or `cameraProxy`), static navigation, thinking (`minimal`, `low`, `medium`, or `high`), source scopes, fps (0.25–24), maximum width (160–1280), window length (1–60 seconds), media/request caps, output token cap (256–8192), and separate media/API worker bounds (1–4 / 1–8). Set the request payload deliberately: `video` excludes audio; `video_and_audio` includes selected source audio. Audio-bearing policies require source audio support. The core validates all limits and policy consistency.

Discover the selected project and asset through `project list` and
`clips list --project PROJECT_ID`. For a four-second selected source, a complete
`policy.json` is:

```json
{
  "projectId": "PROJECT_ID",
  "assetIds": ["ASSET_ID"],
  "provider": "gemini-direct",
  "primaryModel": "gemini-3.5-flash-lite",
  "allowedModels": ["gemini-3.5-flash-lite"],
  "allowFallback": false,
  "payload": "video",
  "includeProjectVocabulary": false,
  "budgetUsd": 1.0,
  "parameters": {
    "kind": "native_video_review",
    "options": {
      "version": 1,
      "purpose": "moments",
      "tier": "flex",
      "representation": "original",
      "navigation": "static",
      "thinking": "minimal",
      "sourceScopes": [{"assetId": "ASSET_ID", "sourceInSeconds": 0.0, "sourceOutSeconds": 4.0}],
      "fps": 1.0,
      "maxWidth": 320,
      "maxWindowSeconds": 4,
      "maxMediaSeconds": 4,
      "maxRequests": 1,
      "maxOutputTokens": 1024,
      "mediaWorkers": 1,
      "apiWorkers": 1
    }
  }
}
```

Choose source bounds from the selected asset's actual duration. These are
explicit source scopes, including for subwindows; they never authorize another
asset or implicit audio/text export. Transcript payloads additionally require
current accepted local speech on the explicitly selected generated proxy; only
whole segments contained in the actual window are attached.

Typical sequence:

```sh
rcdvlog --catalog catalog.sqlite native-video preflight --policy-file policy.json
rcdvlog --catalog catalog.sqlite native-video authorize --policy-file policy.json
rcdvlog --catalog catalog.sqlite native-video pilot --policy-file policy.json
rcdvlog --catalog catalog.sqlite native-video windows --policy-file policy.json
rcdvlog --catalog catalog.sqlite native-video run --policy-id POLICY_ID
rcdvlog --catalog catalog.sqlite native-video status --project PROJECT_ID --run RUN_ID
```

`preflight`, `windows`, `status`, `wait`, `result`, `response`, `evidence-list`, and `run-windows` open the existing catalog read-only. `authorize`, `pilot`, `prepare`, `run`, `cancel`, and `recover` can write. Evidence listing is keyset paginated with `--after` and `--limit`; run-window listing uses `--after-index` and `--limit`. `prepare` consumes one typed `NativeVideoWindowRequest` JSON file and only creates/reuses local prepared media; it does not authorize or contact a provider. `run --windows-file` accepts a JSON array of those requests. `wait` defaults to 60 seconds and is capped at one hour. SIGINT is passed to core cancellation checks; `cancel` requests cancellation for an active run.

No command silently retries, changes providers, adds fallback behavior, or treats a pilot as human acceptance. The core reports estimates separately from confirmed usage and uncertain reserves. Progress is emitted as JSON envelopes on stderr; command results are JSON on stdout. Never place credentials in policy files or window files. Provider request bodies, returned raw media, and secrets are not emitted to CLI stdout.

The first deterministic window is the capability pilot and is reused by the
batch. A cache hit preserves the original request/evidence identity and causes
no new transmission. `moments` runs project into the existing `moments` evidence
owner with explicit native-video provenance; source gaps remain uninspected and
all native times remain proposals. `endpointReview` keeps its assistive results
separate from canonical discovery. Neither purpose confirms human ranges,
changes orientation, edits chronology, activates a plan or certifies words/cuts.

The local carrier uses measured source PTS, rotation baked once, 24 fps and a
separate declared provider sampling rate. Audio is stripped unless explicitly
included. Its PCM sample bounds refer to the selected representation clock;
AAC padding and the final held frame are derived tails excluded by the eligible
local timestamp bound. Unsupported clock mappings fail explicitly.

Accounting reserves the full registered input-context/output ceiling before
sending, using Standard price ceilings even for requested Flex. This needs
about $0.32 of temporary headroom per initial request; sparse-video usage
estimates can be much smaller. Concurrent requests wait for existing reservations
to settle when this headroom is temporarily occupied. Published usage estimates,
confirmed invoices, active reservations and uncertainty are reported separately.
Missing or conflicting model/tier/usage evidence preserves conservative reserves.
No request falls back to Standard or another model, retries automatically, or
claims a cache hit from merely repeating a request.

Completed and rejected bodies are retained unchanged locally. Use
`native-video response --project PROJECT_ID --evidence EVIDENCE_ID` to explicitly
read a bounded response, and `native-video result` for validated candidates,
flags, provenance and rejection reasons. Evidence/run-window pages are capped at
100 rows; run reports include at most 100 evidence IDs and 100 failures, with
separate total counts and paginated durable window references. Status reads do
not repair interrupted owners or turn an older preview/evidence into current
source verification. An interrupted/uncertain transmission requires canonical
receipt reconciliation; it is never resent by starting a fresh run.

After an interrupted local result write, use
`native-video recover --project PROJECT_ID --evidence EVIDENCE_ID`. Recovery
rechecks the original source, carrier and retained response and reconciles its
observed receipt through the existing accounting owner. It requires enough
stored evidence to settle uncertainty; it never infers a free request or
contacts the provider. A recovered capability pilot can qualify the original
policy. Recovery preserves human labels and historical run reports.

Raw response retention is capped at 2 MiB and requires valid UTF-8. Oversized or
invalid responses can leave a dispatched request uncertain without a retained
body. Media preparation and freshness checks share the local heavy-work slot;
the slot is released before HTTP. Native moment projection does not apply the
legacy frame-discovery edit profile; sequence intent is applied later by the
selection workflow.

The direct leaf uses only `GEMINI_API_KEY` in a sensitive HTTP header after
admission. It sends one POST to the registered endpoint, with no redirect,
automatic retry or tier fallback; HTTP timeout is 600 seconds. Cancellation
interrupts local I/O and stops new work, while already-dispatched uncertainty
remains reserved. Optional provider-internal navigation is explicitly blocked
until its actual endpoint/model/tier and bounded tool evidence are qualified.
Generated/fake tests do not establish live capability or human listening quality.
