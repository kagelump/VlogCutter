# Camera supplied proxy sidecars

Some camera cards include a same-name `.LRF` companion beside an original clip.
VlogCutter can discover and register these files without changing either media
file. Discovery is scoped to one card root and reports each source clip as
`candidate`, `registered`, `missing`, `unsupported`, `mismatch`, or
`truncated_or_unreadable`.

```sh
rcdvlog camera-proxy discover --project PROJECT_ID --card /path/to/card
rcdvlog camera-proxy register --asset ASSET_ID --card /path/to/card --sidecar /path/to/card/DCIM/CLIP0001.LRF
rcdvlog camera-proxy status --asset ASSET_ID
```

Registration records the exact card root and sidecar path, byte length, full
file BLAKE3 fingerprint, codec, dimensions, duration, and verification time in
the project catalog. The original and sidecar remain read-only. Registration
requires H.264 video, AAC audio, lower resolution than the source, duration
within one second, and a successful full decode. Discovery alone never changes
the catalog.

Camera sidecars have a separate identity from VlogCutter's generated proxy
recipes. They do not set `assets.proxy_path`, do not count as RawV3, and do not
change generated-proxy readiness. Review selects a generated proxy first, then
an available original, then a registered sidecar whose full-file fingerprint
still matches. Media delivery uses the existing bounded byte-range stream, so
an offline original can still be reviewed from a verified sidecar. A missing or
changed registered file is reported stale and is not selected.
