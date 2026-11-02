import { useEffect, useMemo, useState } from "react";
import type {
  BrowserProjectClient,
  Discussion,
  DiscussionItem,
  DiscussionItemKind,
  ReviewWorkspace,
  WorkspaceUtterance,
} from "@rcdvlog/project-client";
import { StatePanel, StatusBadge } from "@rcdvlog/review-ui";
import { normalizeError } from "../errors";
import { formatStamp } from "./TranscriptPanel";

export const REVIEW_CODE_COPY: Record<string, string> = {
  review_item_retracted: "That suggestion was retracted in the conversation; it cannot be accepted. Record a new candidate edit instead.",
  review_target_unconfirmed: "Confirm which cut this discussion is about before accepting an edit.",
  review_conflicting_acceptance: "Another edit in this discussion is already accepted. Decline or retract it first so accepted edits stay consistent.",
  review_human_required: "Only a signed-in reviewer can do this; agent suggestions stay provisional.",
  review_reconciliation_conflict: "This item is already linked to a different canonical note.",
  invalid_input: "That value is not valid here.",
};

const KIND_LABEL: Record<DiscussionItemKind, string> = {
  observation: "Observation",
  candidateEdit: "Candidate edit",
  speakerClaim: "Speaker claim",
  retraction: "Retraction",
  note: "Note",
  decision: "Decision",
};

const SPEAKER_OPTIONS = ["unknown", "A", "B", "overlap", "tv", "background"];

function describe(reason: unknown) {
  const error = normalizeError(reason);
  return REVIEW_CODE_COPY[error.code] ?? error.message;
}

function movieLabel(u: WorkspaceUtterance) {
  if (!u.movieTime) return "movie time unknown";
  const precision = u.movieTime.source === "humanCorrection" ? "set by you" : `±${u.movieTime.uncertaintySeconds.toFixed(1)} s, ${u.movieTime.source}`;
  return `movie ${formatStamp(u.movieTime.movieSeconds)} (${precision})`;
}

function UtteranceEditor({ utterance, onCorrect }: { utterance: WorkspaceUtterance; onCorrect: (field: "text" | "speaker" | "movieTime" | "excluded", value: unknown) => Promise<void> }) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(utterance.text);
  const [movie, setMovie] = useState(utterance.movieTime ? utterance.movieTime.movieSeconds.toFixed(2) : "");
  return <li className={utterance.excluded ? "tv-utterance suppressed" : "tv-utterance"}>
    <span className="tv-stamp" title="Recording clock">{formatStamp(utterance.machine.startSeconds)}</span>
    <label className="tv-speaker-select"><span className="visually-hidden">Speaker for {utterance.machine.utteranceId}</span>
      <select aria-label={`Speaker for ${utterance.machine.utteranceId}`} value={utterance.speaker} onChange={(event) => void onCorrect("speaker", event.target.value)}>
        {SPEAKER_OPTIONS.map((s) => <option key={s} value={s}>{s === "unknown" ? "Speaker ?" : s === "A" || s === "B" ? `Speaker ${s}` : s}</option>)}
      </select></label>
    <span className="tv-language">{utterance.language || "?"}</span>
    <span className="tv-text">
      {editing ? <form onSubmit={(event) => { event.preventDefault(); void onCorrect("text", text).then(() => setEditing(false)); }}>
        <input aria-label="Corrected text" value={text} onChange={(event) => setText(event.target.value)} />
        <button>Save</button><button type="button" className="secondary" onClick={() => setEditing(false)}>Cancel</button>
      </form> : <>{utterance.displayText}{utterance.text !== utterance.machine.text ? <small className="tv-raw"> machine: {utterance.machine.text}</small> : utterance.displayText !== utterance.text ? <small className="tv-raw"> raw: {utterance.text}</small> : null}</>}
    </span>
    <span className="tv-flags">
      {movieLabel(utterance)}
      {utterance.candidateOperationIds.length ? ` · candidates ${utterance.candidateOperationIds.join(", ")}` : ""}
      {utterance.referenceHint ? ` · refers to the ${utterance.referenceHint} shot?` : ""}
      {utterance.corrections.length ? ` · ${utterance.corrections.length} correction${utterance.corrections.length === 1 ? "" : "s"}` : ""}
      {" "}<button type="button" className="link-button" onClick={() => setEditing(true)}>Edit text</button>
      <form className="tv-inline" onSubmit={(event) => { event.preventDefault(); void onCorrect("movieTime", movie.trim() === "" ? null : Number(movie)); }}>
        <input aria-label={`Movie time for ${utterance.machine.utteranceId}`} value={movie} onChange={(event) => setMovie(event.target.value)} placeholder="movie s" size={6} />
        <button className="link-button">Set time</button>
      </form>
      <button type="button" className="link-button" onClick={() => void onCorrect("excluded", !utterance.excluded)}>{utterance.excluded ? "Include" : "Exclude (TV/background)"}</button>
    </span>
  </li>;
}

function ItemRow({ item, all }: { item: DiscussionItem; all: DiscussionItem[] }) {
  const refers = item.refersToItemId ? all.find((i) => i.itemId === item.refersToItemId) : null;
  return <li className={`tv-item tv-item-${item.status}`} data-kind={item.kind}>
    <strong>{KIND_LABEL[item.kind]}{item.decision ? `: ${item.decision}` : ""}</strong>
    {item.provisional ? <StatusBadge label="Agent suggestion · provisional" tone="warning" /> : null}
    {item.kind !== "decision" && item.kind !== "note" && item.kind !== "retraction" ? <StatusBadge label={item.status} tone={item.status === "accepted" ? "positive" : item.status === "retracted" || item.status === "declined" ? "critical" : "neutral"} /> : null}
    <span>{item.speaker ? `Speaker ${item.speaker}: ` : ""}{item.text}</span>
    {refers ? <small> → {KIND_LABEL[refers.kind]} “{refers.text.slice(0, 60)}”</small> : null}
    {item.canonicalNoteId ? <small> · canonical note {item.canonicalNoteId}</small> : null}
  </li>;
}

function DiscussionCard({ client, discussion, workspace, act }: { client: BrowserProjectClient; discussion: Discussion; workspace: ReviewWorkspace; act: (work: () => Promise<ReviewWorkspace>) => Promise<void> }) {
  const [kind, setKind] = useState<DiscussionItemKind>("observation");
  const [text, setText] = useState("");
  const [speaker, setSpeaker] = useState("unknown");
  const [refers, setRefers] = useState("");
  const [decision, setDecision] = useState<"accepted" | "declined" | "deferred">("accepted");
  const decidable = discussion.items.filter((i) => i.kind === "observation" || i.kind === "candidateEdit" || i.kind === "speakerClaim");
  const utterances = workspace.utterances.filter((u) => discussion.utteranceIds.includes(u.machine.utteranceId));
  const needsRefers = kind === "decision" || kind === "retraction";
  async function submit() {
    await act(() => client.addReviewDiscussionItem(workspace.projectId, discussion.discussionId, {
      kind, authorKind: "human", text: text || (kind === "decision" ? `${decision}` : text),
      speaker: speaker === "unknown" ? null : speaker,
      quoteUtteranceIds: utterances.map((u) => u.machine.utteranceId).slice(0, 32),
      decision: kind === "decision" ? decision : null,
      refersToItemId: needsRefers ? refers || null : null,
    }));
    setText("");
  }
  return <article className="tv-discussion" aria-label={`Discussion ${discussion.title}`}>
    <header>
      <h3>{discussion.title}</h3>
      <span className="help">{discussion.startRecordingSeconds !== null ? `${formatStamp(discussion.startRecordingSeconds)}–${formatStamp(discussion.endRecordingSeconds ?? 0)}` : ""} · {discussion.speakers.map((s) => s === "unknown" ? "?" : s).join("/")} · {discussion.languages.join("+") || "?"} · {discussion.groupingSource === "heuristic" ? "grouped automatically" : "grouped by you"}</span>
    </header>
    <ol className="tv-quotes">{utterances.map((u) => <li key={u.machine.utteranceId} lang={u.language === "zh" ? "zh-Hant" : u.language}>“{u.displayText}”{u.speaker !== "unknown" ? ` — ${u.speaker}` : ""}</li>)}</ol>
    <div className="tv-target">
      <label>Cut this is about
        <select aria-label="Target cut" value={discussion.unanchored ? "__unanchored" : discussion.targetOperationId ?? ""} onChange={(event) => {
          const value = event.target.value;
          void act(() => value === "__unanchored"
            ? client.updateReviewDiscussion(workspace.projectId, discussion.discussionId, { unanchored: true })
            : client.updateReviewDiscussion(workspace.projectId, discussion.discussionId, { targetOperationId: value || null }));
        }}>
          <option value="">Not confirmed</option>
          <option value="__unanchored">Unanchored (general remark)</option>
          {workspace.cuts.map((cut) => <option key={cut.operationId} value={cut.operationId}>{discussion.candidateOperationIds.includes(cut.operationId) ? "★ " : ""}#{cut.position + 1} {cut.operationId} ({formatStamp(cut.movieInSeconds)}–{formatStamp(cut.movieOutSeconds)})</option>)}
        </select></label>
      <p className="help">{discussion.candidateOperationIds.length ? `★ candidates from the estimated movie time: ${discussion.candidateOperationIds.join(", ")}. They are locators only, never chosen for you.` : "No time-based candidates; choose a cut or keep it unanchored."}</p>
    </div>
    <ul className="tv-items">{discussion.items.map((i) => <ItemRow key={i.itemId} item={i} all={discussion.items} />)}</ul>
    <form className="tv-item-form" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
      <select aria-label="Item kind" value={kind} onChange={(event) => setKind(event.target.value as DiscussionItemKind)}>
        {(Object.keys(KIND_LABEL) as DiscussionItemKind[]).map((k) => <option key={k} value={k}>{KIND_LABEL[k]}</option>)}
      </select>
      <select aria-label="Said by" value={speaker} onChange={(event) => setSpeaker(event.target.value)}>
        {SPEAKER_OPTIONS.slice(0, 3).map((s) => <option key={s} value={s}>{s === "unknown" ? "Speaker ?" : `Speaker ${s}`}</option>)}
      </select>
      {needsRefers ? <select aria-label="Refers to" value={refers} onChange={(event) => setRefers(event.target.value)}>
        <option value="">Choose the item…</option>
        {decidable.map((i) => <option key={i.itemId} value={i.itemId}>{KIND_LABEL[i.kind]}: {i.text.slice(0, 40)}</option>)}
      </select> : null}
      {kind === "decision" ? <select aria-label="Decision" value={decision} onChange={(event) => setDecision(event.target.value as typeof decision)}>
        <option value="accepted">Accept</option><option value="declined">Decline</option><option value="deferred">Defer</option>
      </select> : null}
      <input aria-label="Item text" value={text} onChange={(event) => setText(event.target.value)} placeholder={kind === "decision" ? "Why (optional)" : "What was said or meant"} />
      <button disabled={(!text.trim() && kind !== "decision") || (needsRefers && !refers)}>Add</button>
    </form>
  </article>;
}

export function ReviewWorkspaceView({ client, projectId, targetId, sessionId, runId, navigate }: { client: BrowserProjectClient; projectId: string; targetId: string; sessionId: string; runId: string; navigate: (path: string) => void }) {
  const [workspace, setWorkspace] = useState<ReviewWorkspace | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [speakerFilter, setSpeakerFilter] = useState("all");
  const [statusFilter, setStatusFilter] = useState("all");
  const [syncFilter, setSyncFilter] = useState("all");
  useEffect(() => { client.getReviewWorkspace(projectId, runId).then(setWorkspace).catch((reason) => setError(describe(reason))); }, [client, projectId, runId]);

  async function act(work: () => Promise<ReviewWorkspace>) {
    setError(null);
    try { setWorkspace(await work()); } catch (reason) { setError(describe(reason)); }
  }

  const discussions = useMemo(() => (workspace?.discussions ?? []).filter((d) => {
    if (speakerFilter !== "all" && !d.speakers.includes(speakerFilter)) return false;
    if (statusFilter === "decided" && !d.items.some((i) => i.kind === "decision")) return false;
    if (statusFilter === "open" && d.items.some((i) => i.kind === "decision")) return false;
    if (syncFilter === "anchored" && !d.targetOperationId) return false;
    if (syncFilter === "unconfirmed" && (d.targetOperationId || d.unanchored)) return false;
    return true;
  }), [workspace, speakerFilter, statusFilter, syncFilter]);

  if (!workspace) return <section className="workspace-page">{error ? <p className="inline-error" role="alert">{error}</p> : <StatePanel title="Loading review" busy><p>Reading transcript and discussions…</p></StatePanel>}</section>;
  const sessionPath = `/tv-review/${encodeURIComponent(projectId)}/${encodeURIComponent(targetId)}/sessions/${encodeURIComponent(sessionId)}`;
  return <section className="workspace-page tv-review-page">
    <div className="page-heading"><div><p className="eyebrow">Voice review / revision {workspace.revisionId}</p><h1>Editorial review</h1></div>
      <div className="tv-heading-actions">
        <StatusBadge label={workspace.sync.status === "provisional" ? "Movie time: manual cue" : "Movie time: unanchored"} tone="warning" />
        <a className="button-link secondary-link" href={client.reviewReportUrl(projectId, runId)} target="_blank" rel="noreferrer">Agent report (JSON)</a>
        <button className="secondary" onClick={() => navigate(sessionPath)}>Back to session</button>
      </div></div>
    <p className="help">{workspace.sync.note} {workspace.coverage.note}</p>
    {error ? <p className="inline-error" role="alert">{error}</p> : null}
    <div className="tv-review-columns">
      <div>
        <h2 className="tv-section-title">Transcript</h2>
        <ol className="tv-utterances">{workspace.utterances.map((u) => <UtteranceEditor key={u.machine.utteranceId} utterance={u}
          onCorrect={(field, value) => act(() => client.correctReviewUtterance(projectId, runId, u.machine.utteranceId, field, value))} />)}</ol>
      </div>
      <div>
        <div className="tv-heading-actions">
          <h2 className="tv-section-title">Discussions</h2>
          {!workspace.discussions.length ? <button onClick={() => void act(() => client.proposeReviewDiscussions(projectId, runId))}>Group into discussions</button> : null}
        </div>
        <div className="tv-filters" role="group" aria-label="Filter discussions">
          <select aria-label="Speaker filter" value={speakerFilter} onChange={(event) => setSpeakerFilter(event.target.value)}><option value="all">All speakers</option><option value="A">A</option><option value="B">B</option><option value="unknown">Unknown</option></select>
          <select aria-label="Status filter" value={statusFilter} onChange={(event) => setStatusFilter(event.target.value)}><option value="all">Any status</option><option value="open">Undecided</option><option value="decided">Has a decision</option></select>
          <select aria-label="Target filter" value={syncFilter} onChange={(event) => setSyncFilter(event.target.value)}><option value="all">Any target</option><option value="anchored">Cut confirmed</option><option value="unconfirmed">Cut not confirmed</option></select>
        </div>
        {discussions.map((d) => <DiscussionCard key={d.discussionId} client={client} discussion={d} workspace={workspace} act={act} />)}
        {workspace.discussions.length && !discussions.length ? <p className="help">No discussion matches these filters.</p> : null}
      </div>
    </div>
  </section>;
}
