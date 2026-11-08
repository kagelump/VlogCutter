import { ProjectClientError } from "@rcdvlog/project-client";

/**
 * User-facing copy for stable server error codes. The UI never parses server
 * prose to pick a state; it keys on `code` and falls back to the message.
 */
export const ERROR_COPY: Record<string, string> = {
  invalid_input: "That value isn't valid here.",
  idempotency_conflict: "This request conflicts with an earlier one.",
  low_disk_space: "The server is low on disk space.",
  project_replica_not_found: "This project isn't on the server.",

  // Review-target import (#221, #232)
  review_target_unsupported_version: "This handoff was exported by a different VlogCutter version. Re-export it with a matching build.",
  review_target_digest_mismatch: "The handoff was edited after export. Re-export it on the Mac.",
  review_target_invalid: "The handoff is malformed or its cut list is incomplete.",
  review_target_artifact_not_verified: "The preview movie wasn't fully verified. Re-render or re-verify it, then export again.",
  review_target_verification_expired: "The verification is out of date. Re-export on the Mac to refresh it.",
  review_target_artifact_unavailable: "The preview movie isn't known to be present and unchanged on the Mac.",
  review_target_project_mismatch: "This handoff belongs to a different project.",
  review_target_conflict: "A different version of this movie was already imported. Nothing was overwritten.",
  review_target_contract_conflict: "This movie was already imported as a different review type. Open the existing one instead.",
  review_target_historical_contract: "This is a historical review and is imported separately.",
  review_target_historical_acknowledgement_required: "Confirm the historical-review limits before importing.",
  review_target_not_historical: "This is a current review, not a historical one.",

  // Recording sessions (#222)
  review_session_busy: "The server is busy. Uploads will retry automatically.",
  review_session_not_recording: "This recording has already ended.",
  review_session_chunk_conflict: "Some audio conflicts with what the server already has. The local copy is kept.",
  review_session_chunk_corrupt: "Some audio failed its integrity check. The local copy is kept.",
  review_session_limit_exceeded: "This recording reached its size or length limit.",
  review_session_event_storage_failed: "Some recording events couldn't be saved. Keep this recording for recovery.",
  review_session_audio_unavailable: "Audio can only be downloaded from a completed recording.",

  // Transcription (#224)
  review_transcription_session_not_finalized: "Stop the recording and let it finish uploading first.",
  review_transcription_no_audio: "This recording has no audio.",
  review_transcription_local_model_unavailable: "Speech models aren't installed on this server. Run `rcdvlog models install`, or use the cloud option.",
  review_transcription_cloud_not_authorized: "Confirm that this recording may be sent to the provider.",
  review_transcription_budget_required: "Set a spending limit.",
  review_transcription_provider_unavailable: "No OpenRouter key is configured on this server.",
  review_transcription_budget_exhausted: "The spending limit was reached. Raise it and retry to continue.",
  review_transcription_dispatch_outcome_unknown: "A cloud response was lost. It counts as spent and won't be resent.",
  review_transcription_audio_changed: "The recording changed after transcription started.",
  review_transcription_cancelled: "Cancelled. Finished parts are kept; retry to resume.",
  review_transcription_partial: "Some parts couldn't be transcribed. Finished parts are kept.",
  review_transcription_failed: "Transcription failed. The recording is unaffected.",
  diarization_unavailable: "Speakers aren't identified automatically. Set them yourself where it matters.",
  tv_dialogue_not_separated: "TV dialogue isn't separated from the room, so lines from the video may appear.",
  recording_has_gaps: "The recording has gaps. They're skipped, not filled.",
  budget_below_worst_case: "The limit is below the worst case. The run may stop early and can be resumed.",

  // Editorial review (#225)
  review_item_retracted: "That suggestion was retracted and can't be accepted.",
  review_target_unconfirmed: "Pick the shot this discussion is about before accepting an edit.",
  review_conflicting_acceptance: "Another edit in this discussion is already accepted. Decline it first.",
  review_human_required: "Only a signed-in reviewer can do this.",
  review_reconciliation_conflict: "This item is already linked to a different note.",
};

const KIND_COPY: Record<ProjectClientError["kind"], string> = {
  server_unavailable: "Can't reach the server. Check your connection.",
  session_expired: "Your session expired. Sign in again.",
  credential_revoked: "This browser's access was revoked.",
  csrf_failure: "Your session token expired. Sign in again.",
  protocol_mismatch: "This page is out of date for the server. Reload to update.",
  server_error: "Something went wrong on the server.",
};

export function toClientError(error: unknown): ProjectClientError {
  if (error instanceof ProjectClientError) return error;
  return new ProjectClientError("server_error", "Something went wrong.", "unexpected_error");
}

/** One readable sentence for any error or stable code. */
export function describeError(error: unknown): string {
  const clientError = toClientError(error);
  if (clientError.code && ERROR_COPY[clientError.code]) return ERROR_COPY[clientError.code];
  if (clientError.kind !== "server_error") return KIND_COPY[clientError.kind];
  return clientError.message || KIND_COPY.server_error;
}

export function describeCode(code: string): string {
  return ERROR_COPY[code] ?? humanize(code);
}

export function isAuthError(error: unknown) {
  return error instanceof ProjectClientError
    && (error.kind === "session_expired" || error.kind === "credential_revoked" || error.kind === "csrf_failure");
}

export function humanize(code: string) {
  const text = code.replace(/[_:]+/g, " ").replace(/([a-z])([A-Z])/g, "$1 $2").toLowerCase().trim();
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** "exiftool not found", "Backup freshness: stale". For readiness checks. */
export function describeCheck(check: { name: string; status: string; code?: string }) {
  if (check.name.startsWith("tool_") && check.code === "tool_missing") return `${check.name.slice(5)} not found`;
  const detail = check.code ? humanize(check.code).toLowerCase() : check.status;
  return `${humanize(check.name)}: ${detail}`;
}
