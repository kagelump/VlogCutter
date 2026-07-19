# Integration testing

The automated integration suite exercises the MVP through the same compiled CLI and SQLite catalog used by agents and the desktop application. It deliberately avoids mocks at the process and media-tool boundaries.

## Automated coverage

| Boundary | Scenarios |
| --- | --- |
| CLI process contract | Independent invocations, persistent catalog selection, JSON stdout/stderr envelopes, exit status, and stable error codes |
| Catalog concurrency | A read-only CLI process remains responsive while another SQLite connection holds an uncommitted write transaction in WAL mode |
| Media workflow | Real FFmpeg fixture creation, ffprobe ingest, non-media filtering, progress events, idempotent re-ingest, metadata persistence, notes/review/include edits, clock correction, proxy generation, offline-original review, and OTIO export |

Run the integration suite with:

```bash
pnpm test:integration
```

FFmpeg and ffprobe are required for the media workflow. The process and concurrency tests still run if either tool is unavailable; the media test reports that it was skipped. `pnpm check` also includes these tests through the Rust workspace suite.

## Manual and system-level acceptance

These behaviors need their real host application or hardware and should not be represented by misleading headless mocks:

- Import the generated `.otio` into the current DaVinci Resolve release and inspect media links, source ranges, hard-cut order, and retained metadata.
- Exercise the Tauri folder dialog, macOS filesystem permission prompts, native media playback, and laptop sleep/wake behavior.
- Run the Expo-scale fixture from a mounted NAS, including disconnect/reconnect during ingest and proxy generation.
- Confirm acceptable scan, proxy, catalog, and UI performance over roughly 100 hours and thousands of 4K/60 clips.

The checked-in media integration fixture is intentionally one second and one source. Scale and Resolve compatibility belong in periodic acceptance runs, not every local or CI test.
