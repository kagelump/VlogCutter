# Implementation status

Versions: project schema 66, server control schema 17, sync protocol 1.0, backup format 1.

- **Legacy desktop surface.** Labels, notes, the review queue, the story ribbon and the Chronology desk are leftovers from the desktop-first CUJ. They still work, but they are not the direction (see [desktop.md](desktop.md)).
- **No project-local agent workspace yet** (#48). The CLI only binds/checks `.vlogcutter/cli-binding.json`; nothing generates `PROJECT.md`, `script.md` or `edit-plan.json`.
- **Edit plans never self-activate.** Revisions are immutable and new drafts are inactive. Feedback changes a plan only through an explicitly accepted proposal, which yields another inactive draft.
- **Moments are not cuts.** Moments, key moments and trim suggestions are sparse suggestions; none of them certifies a source range. There are no chapter, event or script entities.
- **Preview renders are node-local evidence**, not synced decisions. Full-range sources are normalized to TV range (preview identity v2). Historical rendered-movie reviews are a separate read-only path and never count as a strict handoff.
- **OTIO export refuses** plans whose audio edits (J/L, fades, explicit audio ranges) it cannot yet preserve. Resolve import has never been accepted on real hardware.
- **Writes are full patches.** `update_asset` replaces the whole annotation, guarded by `expectedRevision`, so a same-field race is not resolved per field.
- **AI suggestions have no accept/reject.** A forced re-analysis overwrites AI titles and descriptions. Human notes are never touched.
- **Paid providers fail closed.** Token estimates draw from the same budget as real charges, and native-video review has no automatic model, tier or provider fallback.
- **TV review lives on the server** (control schemas 13–17). It never writes the project catalog; review notes come back only through the idempotent `edit-plan feedback record-once`.
- **Search is plain filtering, not FTS.** Only flattened transcript text is indexed, so search matches carry no timestamps.
- **Chronology is explicit.** Import proposes capture-sequence splits but never applies them. Story beats are not persisted, and clip-exception and event-order controls exist only in the API and CLI.
- **Unrun manual acceptance:** physical card import, native playback through the loopback media origin, sleep/wake, NAS scale, Ubuntu/Compose, NFS/SMB outages, Caddy/Tailscale HTTPS, macOS signing/notarization. The last server acceptance run was Task 19 (`git show 4c5eb2f:docs/tasks/server/ACCEPTANCE_TASK_19.md`).
