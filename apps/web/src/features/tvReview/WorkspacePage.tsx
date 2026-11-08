import { useMemo, useState, type FormEvent } from "react";
import { useParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { Discussion, DiscussionItem, DiscussionItemKind, ReviewWorkspace, WorkspaceUtterance } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { paths } from "../../app/paths";
import { Page } from "../../app/Shell";
import { formatStamp } from "../../lib/format";
import { useDocumentTitle, useMediaQuery } from "../../lib/hooks";
import { Badge, Button, ButtonAnchor, Disclosure, EmptyState, ErrorMessage, Loading, Notice, PageHeader, Segmented, type Tone } from "../../ui";
import { useProjectId } from "../projects/ProjectLayout";
import styles from "./TvReview.module.css";

const KIND_LABEL: Record<DiscussionItemKind, string> = {
  observation: "Observation",
  candidateEdit: "Suggested edit",
  speakerClaim: "Speaker claim",
  retraction: "Retraction",
  note: "Note",
  decision: "Decision",
};

const STATUS_TONE: Record<DiscussionItem["status"], Tone> = {
  open: "neutral",
  accepted: "positive",
  declined: "critical",
  deferred: "warning",
  retracted: "critical",
};

const SPEAKERS = ["unknown", "A", "B", "overlap", "tv", "background"];
const speakerLabel = (speaker: string) => speaker === "unknown" ? "Speaker ?" : speaker === "A" || speaker === "B" ? `Speaker ${speaker}` : speaker === "tv" ? "TV" : speaker.charAt(0).toUpperCase() + speaker.slice(1);

const SYNC_LABEL: Record<ReviewWorkspace["sync"]["status"], string> = {
  pending: "Timing not matched yet",
  noAnchors: "No timing anchors",
  mapped: "Timing matched",
  failed: "Timing match failed",
};

type Act = (work: () => Promise<ReviewWorkspace>) => void;

export function WorkspacePage() {
  const projectId = useProjectId();
  const { targetId = "", sessionId = "", runId = "" } = useParams();
  const client = useClient();
  const queryClient = useQueryClient();
  const key = keys.workspace(projectId, runId);
  const workspace = useQuery({ queryKey: key, queryFn: () => client.getReviewWorkspace(projectId, runId) });
  const mutation = useMutation({
    mutationFn: (work: () => Promise<ReviewWorkspace>) => work(),
    onSuccess: (next) => queryClient.setQueryData(key, next),
  });
  const act: Act = (work) => mutation.mutate(work);
  const wide = useMediaQuery("(min-width: 1000px)");
  const [pane, setPane] = useState<"discussions" | "transcript">("discussions");
  useDocumentTitle("Review notes");

  const data = workspace.data;
  if (workspace.isPending) return <Page><Loading /></Page>;
  if (!data) return <Page><ErrorMessage error={workspace.error} onRetry={() => void workspace.refetch()} /></Page>;

  const discussionsPane = <Discussions workspace={data} act={act} busy={mutation.isPending} />;
  const transcriptPane = (
    <section aria-label="Transcript">
      {wide ? <h2 className={styles.paneTitle}>Transcript</h2> : null}
      <ol className={styles.utterances}>
        {data.utterances.map((utterance) => (
          <UtteranceEditor
            key={utterance.machine.utteranceId}
            utterance={utterance}
            onCorrect={(field, value) => act(() => client.correctReviewUtterance(projectId, runId, utterance.machine.utteranceId, field, value))}
          />
        ))}
      </ol>
    </section>
  );

  return (
    <Page>
      <PageHeader
        back={{ to: paths.session(projectId, targetId, sessionId), label: "Recording" }}
        title="Review notes"
        titleExtra={<Badge tone={data.sync.status === "mapped" ? "positive" : "warning"}>{SYNC_LABEL[data.sync.status]}</Badge>}
        actions={<ButtonAnchor variant="ghost" icon="external" href={client.reviewReportUrl(projectId, runId)} target="_blank" rel="noreferrer">Agent report</ButtonAnchor>}
      />

      <div className={styles.workspaceNotes}>
        {data.targetContract === "historicalRenderedArtifactReviewV1" ? (
          <Notice tone="warning" title="Historical cut.">These notes describe an already rendered movie and can’t render, edit or activate anything.</Notice>
        ) : null}
        <ErrorMessage error={mutation.error} />
        <Disclosure title="How comments are matched to shots">
          <p className={styles.muted}>{data.sync.note} {data.coverage.note}</p>
        </Disclosure>
      </div>

      {wide ? (
        <div className={styles.workspaceGrid}>
          {discussionsPane}
          <div className={styles.transcriptColumn}>{transcriptPane}</div>
        </div>
      ) : (
        <>
          <div className={styles.paneSwitch}>
            <Segmented
              label="Show"
              value={pane}
              onChange={setPane}
              options={[
                { value: "discussions", label: `Discussions (${data.discussions.length})` },
                { value: "transcript", label: `Transcript (${data.utterances.length})` },
              ]}
            />
          </div>
          {pane === "discussions" ? discussionsPane : transcriptPane}
        </>
      )}
    </Page>
  );
}

/* ---------- Discussions ---------- */

function Discussions({ workspace, act, busy }: { workspace: ReviewWorkspace; act: Act; busy: boolean }) {
  const client = useClient();
  const [speaker, setSpeaker] = useState("all");
  const [status, setStatus] = useState("all");
  const [shot, setShot] = useState("all");
  const visible = useMemo(() => workspace.discussions.filter((discussion) => {
    const decided = discussion.items.some((item) => item.kind === "decision");
    if (speaker !== "all" && !discussion.speakers.includes(speaker)) return false;
    if (status === "decided" && !decided) return false;
    if (status === "open" && decided) return false;
    if (shot === "set" && !discussion.targetOperationId) return false;
    if (shot === "unset" && (discussion.targetOperationId || discussion.unanchored)) return false;
    return true;
  }), [workspace.discussions, speaker, status, shot]);

  if (!workspace.discussions.length) {
    return (
      <section aria-label="Discussions">
        <EmptyState
          title="No discussions yet"
          action={<Button variant="primary" busy={busy} onClick={() => act(() => client.proposeReviewDiscussions(workspace.projectId, workspace.runId))}>Group transcript into discussions</Button>}
        >
          Group related lines of the transcript so each topic can be tied to a shot and decided on.
        </EmptyState>
      </section>
    );
  }

  return (
    <section aria-label="Discussions" className={styles.discussions}>
      <div className={styles.filters} role="group" aria-label="Filter discussions">
        <select aria-label="Speaker" value={speaker} onChange={(event) => setSpeaker(event.target.value)}>
          <option value="all">All speakers</option><option value="A">Speaker A</option><option value="B">Speaker B</option><option value="unknown">Unknown speaker</option>
        </select>
        <select aria-label="Status" value={status} onChange={(event) => setStatus(event.target.value)}>
          <option value="all">Any status</option><option value="open">Undecided</option><option value="decided">Decided</option>
        </select>
        <select aria-label="Shot" value={shot} onChange={(event) => setShot(event.target.value)}>
          <option value="all">Any shot</option><option value="set">Shot chosen</option><option value="unset">Shot not chosen</option>
        </select>
      </div>
      {visible.map((discussion) => <DiscussionCard key={discussion.discussionId} discussion={discussion} workspace={workspace} act={act} />)}
      {!visible.length ? <p className={styles.muted}>No discussions match these filters.</p> : null}
    </section>
  );
}

function DiscussionCard({ discussion, workspace, act }: { discussion: Discussion; workspace: ReviewWorkspace; act: Act }) {
  const client = useClient();
  const quotes = workspace.utterances.filter((utterance) => discussion.utteranceIds.includes(utterance.machine.utteranceId));
  const candidates = new Set(discussion.candidateOperationIds);
  const cutOptions = [
    ...workspace.cuts.filter((cut) => candidates.has(cut.operationId)),
    ...workspace.cuts.filter((cut) => !candidates.has(cut.operationId)),
  ];
  const range = discussion.startRecordingSeconds !== null ? `${formatStamp(discussion.startRecordingSeconds)}–${formatStamp(discussion.endRecordingSeconds ?? discussion.startRecordingSeconds)}` : null;
  const meta = [
    range,
    discussion.speakers.map((speaker) => speaker === "unknown" ? "?" : speaker).join(" / "),
    discussion.languages.join(" + ") || null,
    discussion.groupingSource === "heuristic" ? "auto-grouped" : null,
  ].filter(Boolean).join(" · ");

  return (
    <article className={styles.discussion} aria-label={`Discussion ${discussion.title}`}>
      <header>
        <h3>{discussion.title}</h3>
        <p className={styles.muted}>{meta}</p>
      </header>
      {quotes.length ? (
        <ol className={styles.quotes}>
          {quotes.map((utterance) => (
            <li key={utterance.machine.utteranceId} lang={utterance.language === "zh" ? "zh-Hant" : utterance.language || undefined}>
              {utterance.speaker !== "unknown" ? <b>{utterance.speaker}</b> : null}{utterance.displayText}
            </li>
          ))}
        </ol>
      ) : null}

      <label className={styles.shotPicker}>
        <span>Shot</span>
        <select
          aria-label="Shot this is about"
          value={discussion.unanchored ? "__general" : discussion.targetOperationId ?? ""}
          onChange={(event) => {
            const value = event.target.value;
            act(() => value === "__general"
              ? client.updateReviewDiscussion(workspace.projectId, discussion.discussionId, { unanchored: true })
              : client.updateReviewDiscussion(workspace.projectId, discussion.discussionId, { targetOperationId: value || null }));
          }}
        >
          <option value="">Not chosen</option>
          <option value="__general">General comment (no shot)</option>
          {cutOptions.map((cut) => (
            <option key={cut.operationId} value={cut.operationId}>
              {candidates.has(cut.operationId) ? "★ " : ""}#{cut.position + 1} at {formatStamp(cut.movieInSeconds)}
            </option>
          ))}
        </select>
      </label>
      {candidates.size ? <p className={styles.hintSmall}>★ marks shots playing around when this was said. They’re hints only.</p> : null}

      {discussion.items.length ? (
        <ul className={styles.items}>
          {discussion.items.map((item) => <ItemRow key={item.itemId} item={item} all={discussion.items} />)}
        </ul>
      ) : null}
      <AddItemForm discussion={discussion} workspace={workspace} quoteIds={quotes.map((utterance) => utterance.machine.utteranceId)} act={act} />
    </article>
  );
}

function ItemRow({ item, all }: { item: DiscussionItem; all: DiscussionItem[] }) {
  const refers = item.refersToItemId ? all.find((other) => other.itemId === item.refersToItemId) : null;
  const showStatus = item.kind === "observation" || item.kind === "candidateEdit" || item.kind === "speakerClaim";
  return (
    <li className={styles.item} data-status={item.status}>
      <div className={styles.itemHead}>
        <strong>{KIND_LABEL[item.kind]}{item.decision ? `: ${item.decision}` : ""}</strong>
        {item.provisional ? <Badge tone="warning" title="Suggested by an agent; not yet confirmed by you">Agent</Badge> : null}
        {showStatus && item.status !== "open" ? <Badge tone={STATUS_TONE[item.status]}>{item.status}</Badge> : null}
      </div>
      {item.text ? <p>{item.speaker ? <span className={styles.muted}>{speakerLabel(item.speaker)}: </span> : null}{item.text}</p> : null}
      {refers ? <p className={styles.hintSmall}>Re: {KIND_LABEL[refers.kind].toLowerCase()} “{refers.text.slice(0, 80)}”</p> : null}
    </li>
  );
}

function AddItemForm({ discussion, workspace, quoteIds, act }: { discussion: Discussion; workspace: ReviewWorkspace; quoteIds: string[]; act: Act }) {
  const client = useClient();
  const [kind, setKind] = useState<DiscussionItemKind>("observation");
  const [text, setText] = useState("");
  const [speaker, setSpeaker] = useState("unknown");
  const [refers, setRefers] = useState("");
  const [decision, setDecision] = useState<"accepted" | "declined" | "deferred">("accepted");
  const decidable = discussion.items.filter((item) => item.kind === "observation" || item.kind === "candidateEdit" || item.kind === "speakerClaim");
  const needsRefers = kind === "decision" || kind === "retraction";

  function submit(event: FormEvent) {
    event.preventDefault();
    act(() => client.addReviewDiscussionItem(workspace.projectId, discussion.discussionId, {
      kind,
      authorKind: "human",
      text: text || (kind === "decision" ? decision : text),
      speaker: speaker === "unknown" ? null : speaker,
      quoteUtteranceIds: quoteIds.slice(0, 32),
      decision: kind === "decision" ? decision : null,
      refersToItemId: needsRefers ? refers || null : null,
    }));
    setText("");
  }

  return (
    <form className={styles.addItem} onSubmit={submit}>
      <select aria-label="Type" value={kind} onChange={(event) => setKind(event.target.value as DiscussionItemKind)}>
        {(Object.keys(KIND_LABEL) as DiscussionItemKind[]).map((value) => <option key={value} value={value}>{KIND_LABEL[value]}</option>)}
      </select>
      {needsRefers ? (
        <select aria-label="Responding to" value={refers} onChange={(event) => setRefers(event.target.value)}>
          <option value="">About which item?</option>
          {decidable.map((item) => <option key={item.itemId} value={item.itemId}>{KIND_LABEL[item.kind]}: {item.text.slice(0, 40)}</option>)}
        </select>
      ) : null}
      {kind === "decision" ? (
        <select aria-label="Decision" value={decision} onChange={(event) => setDecision(event.target.value as typeof decision)}>
          <option value="accepted">Accept</option><option value="declined">Decline</option><option value="deferred">Defer</option>
        </select>
      ) : null}
      <select aria-label="Said by" value={speaker} onChange={(event) => setSpeaker(event.target.value)}>
        {SPEAKERS.slice(0, 3).map((value) => <option key={value} value={value}>{speakerLabel(value)}</option>)}
      </select>
      <input aria-label="Text" value={text} onChange={(event) => setText(event.target.value)} placeholder={kind === "decision" ? "Reason (optional)" : "What was said or meant"} />
      <Button type="submit" disabled={(!text.trim() && kind !== "decision") || (needsRefers && !refers)}>Add</Button>
    </form>
  );
}

/* ---------- Transcript ---------- */

type CorrectionField = "text" | "speaker" | "movieTime" | "excluded";

/** Only a known movie time is worth a line; the page header already says when timing is missing. */
function movieTimeLabel(utterance: WorkspaceUtterance) {
  if (!utterance.movieTime) return null;
  const time = formatStamp(utterance.movieTime.movieSeconds);
  if (utterance.movieTime.source === "humanCorrection") return `movie ${time} (set by you)`;
  const partial = utterance.sync.state === "mixed" ? ", later part uncertain" : "";
  return `movie ${time} ±${utterance.movieTime.uncertaintySeconds.toFixed(1)}s${partial}`;
}

function UtteranceEditor({ utterance, onCorrect }: { utterance: WorkspaceUtterance; onCorrect: (field: CorrectionField, value: unknown) => void }) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(utterance.text);
  const [movie, setMovie] = useState(utterance.movieTime ? utterance.movieTime.movieSeconds.toFixed(2) : "");
  const id = utterance.machine.utteranceId;
  const edited = utterance.text !== utterance.machine.text;
  const details = [
    movieTimeLabel(utterance),
    utterance.referenceHint ? `maybe the ${utterance.referenceHint} shot` : null,
    utterance.excluded ? "excluded" : null,
  ].filter(Boolean);

  function save(event: FormEvent) {
    event.preventDefault();
    if (text !== utterance.text) onCorrect("text", text);
    const current = utterance.movieTime ? utterance.movieTime.movieSeconds.toFixed(2) : "";
    if (movie.trim() !== current) onCorrect("movieTime", movie.trim() === "" ? null : Number(movie));
    setEditing(false);
  }

  function open() {
    setText(utterance.text);
    setMovie(utterance.movieTime ? utterance.movieTime.movieSeconds.toFixed(2) : "");
    setEditing(true);
  }

  return (
    <li className={styles.utterance} data-suppressed={utterance.excluded || undefined}>
      <span className={styles.stamp} title="Time in the recording">{formatStamp(utterance.machine.startSeconds)}</span>
      <select className={styles.speakerSelect} title="Who said this" aria-label={`Speaker for ${id}`} value={utterance.speaker} onChange={(event) => onCorrect("speaker", event.target.value)}>
        {SPEAKERS.map((value) => <option key={value} value={value}>{value === "unknown" ? "?" : value === "tv" ? "TV" : value === "background" ? "BG" : value === "overlap" ? "A+B" : value}</option>)}
      </select>
      <div className={styles.utteranceText}>
        {editing ? (
          <form className={styles.utteranceForm} onSubmit={save}>
            <input aria-label="Corrected text" value={text} autoFocus onChange={(event) => setText(event.target.value)} />
            <label className={styles.movieField}>
              Movie time (s)
              <input aria-label={`Movie time for ${id}`} inputMode="decimal" value={movie} onChange={(event) => setMovie(event.target.value)} placeholder="auto" />
            </label>
            <div className={styles.inlineActions}>
              <Button size="sm" type="submit" variant="primary">Save</Button>
              <Button size="sm" variant="ghost" onClick={() => setEditing(false)}>Cancel</Button>
            </div>
          </form>
        ) : (
          <>
            <span lang={utterance.language === "zh" ? "zh-Hant" : utterance.language || undefined}>{utterance.displayText}</span>
            {edited
              ? <small className={styles.flags}>was: {utterance.machine.text}</small>
              : utterance.displayText !== utterance.text ? <small className={styles.flags}>original: {utterance.text}</small> : null}
            {details.length ? <small className={styles.flags}>{details.join(" · ")}</small> : null}
          </>
        )}
      </div>
      {!editing ? (
        <div className={styles.utteranceActions}>
          <Button size="sm" variant="ghost" onClick={open}>Edit</Button>
          <Button size="sm" variant="ghost" title={utterance.excluded ? "Include this line again" : "Exclude lines from the TV or background"} onClick={() => onCorrect("excluded", !utterance.excluded)}>
            {utterance.excluded ? "Include" : "Exclude"}
          </Button>
        </div>
      ) : null}
    </li>
  );
}
