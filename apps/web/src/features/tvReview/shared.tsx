import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { useQueryClient } from "@tanstack/react-query";
import type { BrowserProjectClient, ReviewSessionSummary, ReviewTargetSummary } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { shortId } from "../../lib/format";
import { Badge, Button, Notice } from "../../ui";
import { defaultOutbox, type ChunkOutbox, type OutboxSession } from "./recording/outbox";
import { VoiceRecording, finalizeWhenDrained, listMicrophones, pendingRecoveries } from "./recording/recorder";

/* ---------- Browser audio capabilities (injectable for tests) ---------- */

export interface VoiceDeps {
  outbox: ChunkOutbox;
  getUserMedia(constraints: MediaStreamConstraints): Promise<MediaStream>;
  listMicrophones(): Promise<MediaDeviceInfo[]>;
  startRecording(options: Parameters<typeof VoiceRecording.start>[0]): Promise<Pick<VoiceRecording, "session" | "cue" | "stop" | "pageHidden" | "observePlayback">>;
}

let browserDeps: VoiceDeps | null = null;
export function browserVoiceDeps(): VoiceDeps {
  browserDeps ??= {
    outbox: defaultOutbox(),
    getUserMedia: (constraints) => navigator.mediaDevices.getUserMedia(constraints),
    listMicrophones,
    startRecording: (options) => VoiceRecording.start(options),
  };
  return browserDeps;
}

const VoiceContext = createContext<VoiceDeps | null>(null);
export function VoiceDepsProvider({ deps, children }: { deps: VoiceDeps; children: ReactNode }) {
  return <VoiceContext.Provider value={deps}>{children}</VoiceContext.Provider>;
}
export function useVoiceDeps(): VoiceDeps {
  return useContext(VoiceContext) ?? browserVoiceDeps();
}

/* ---------- Labels ---------- */

export const isHistorical = (target: { contract?: string }) => target.contract === "historicalRenderedArtifactReviewV1";

/** Prefer editorial names; legacy imports retain a stable revision fallback. */
export function targetTitle(target: Pick<ReviewTargetSummary, "revisionId" | "displayName">) {
  return target.displayName?.trim() || `Cut ${shortId(target.revisionId)}`;
}

export function ReadinessBadge({ target }: { target: ReviewTargetSummary }) {
  if (target.readiness.ready) return <Badge tone="positive">Ready</Badge>;
  if (target.readiness.freshness !== "fresh") return <Badge tone="warning">Verification expired</Badge>;
  return <Badge tone="critical">Movie unavailable</Badge>;
}

export function HistoricalBadge({ target }: { target: { contract?: string } }) {
  return isHistorical(target) ? <Badge tone="warning" title="Integrity-checked only. Current sources and renderability aren't established.">Historical</Badge> : null;
}

export function SessionBadge({ session }: { session: ReviewSessionSummary }) {
  switch (session.state) {
    case "completed": return <Badge tone="positive">Completed</Badge>;
    case "recording": return <Badge tone="warning">{session.abandoned ? "Abandoned" : "Uploading"}</Badge>;
    case "incomplete": return <Badge tone="warning">Incomplete</Badge>;
    case "corrupt": return <Badge tone="critical">Corrupt</Badge>;
    default: return <Badge>Deleted</Badge>;
  }
}

/* ---------- Unfinished uploads ---------- */

/** Finish uploading recordings that a closed tab or dropped connection left in this browser. */
export function PendingRecoveries({ projectId, exclude }: { projectId: string; exclude?: string | null }) {
  const client = useClient();
  const deps = useVoiceDeps();
  const queryClient = useQueryClient();
  const [pending, setPending] = useState<OutboxSession[]>([]);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const refresh = () => pendingRecoveries(deps.outbox, projectId)
    .then((items) => setPending(items.filter((item) => item.sessionId !== exclude)))
    .catch(() => setPending([]));
  useEffect(() => { void refresh(); }, [projectId, exclude]);

  async function resume() {
    setBusy(true);
    const finished = await resumeAll(client, deps.outbox, pending);
    setBusy(false);
    setMessage(finished === pending.length ? "Upload finished." : "Some audio is still waiting to upload. Try again when the server is reachable.");
    await refresh();
    void queryClient.invalidateQueries({ queryKey: keys.project(projectId) });
  }

  if (!pending.length) return message ? <Notice>{message}</Notice> : null;
  return (
    <Notice
      tone="warning"
      title={pending.length === 1 ? "A recording hasn’t finished uploading." : `${pending.length} recordings haven’t finished uploading.`}
      actions={<Button variant="primary" size="sm" busy={busy} onClick={() => void resume()}>Finish upload</Button>}
    >
      The audio is saved in this browser. Upload it before clearing browser data.
    </Notice>
  );
}

async function resumeAll(client: BrowserProjectClient, outbox: ChunkOutbox, sessions: OutboxSession[]) {
  let finished = 0;
  for (const meta of sessions) {
    if (await finalizeWhenDrained(client, outbox, meta, undefined, 60_000)) finished += 1;
  }
  return finished;
}
