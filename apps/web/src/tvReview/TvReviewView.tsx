import { useEffect, useState, type ChangeEvent, type FormEvent } from "react";
import {
  ProjectClientError,
  type BrowserProjectClient,
  type ProjectSummary,
  type ReviewTargetDetail,
  type ReviewTargetPreflight,
  type ReviewTargetSummary,
} from "@rcdvlog/project-client";
import { StatePanel, StatusBadge } from "@rcdvlog/review-ui";
import { InlineError, normalizeError } from "../errors";
import { parseYoutubeVideoId, unwrapHandoff } from "./youtube";

/// Copy for the stable codes a review-target preflight/import can return.
/// Keyed by code; the UI never parses server prose to choose a state.
export const REVIEW_TARGET_CODE_COPY: Record<string, string> = {
  review_target_unsupported_version: "This handoff uses a schema version this server does not import. Re-export it with a matching VlogCutter build.",
  review_target_digest_mismatch: "The handoff was modified after export: its target digest no longer matches. Re-export it on the owning Mac.",
  review_target_invalid: "The handoff is malformed or its cut mapping is incomplete. Nothing was imported.",
  review_target_artifact_not_verified: "The owning host did not fully verify the preview movie. Re-render or re-verify, then export again.",
  review_target_verification_expired: "The verification receipt is too old (or in the future). Re-export on the owning host to refresh it.",
  review_target_artifact_unavailable: "The preview movie is not known to be present and unmodified on its owning host.",
  review_target_project_mismatch: "This handoff belongs to a different project. Open the matching project and import it there.",
  review_target_conflict: "This viewed artifact was already imported with different bytes, plan, revision or cuts. Nothing was overwritten.",
  project_replica_not_found: "This project is not registered on the server.",
  invalid_input: "The request was not valid.",
};

function describe(error: ProjectClientError): string {
  return (error.code && REVIEW_TARGET_CODE_COPY[error.code]) || error.message;
}

export function TvReviewView({ client, path, navigate }: { client: BrowserProjectClient; path: string; navigate: (path: string) => void }) {
  const parts = path.split("/").filter(Boolean).slice(1).map(decodeURIComponent);
  const [projectId, targetId] = parts;
  if (projectId && targetId) return <ReviewTargetDetailView client={client} projectId={projectId} reviewTargetId={targetId} navigate={navigate} />;
  if (projectId) return <ProjectReviewTargets client={client} projectId={projectId} navigate={navigate} />;
  return <ProjectPicker client={client} navigate={navigate} />;
}

function targetPath(projectId: string, reviewTargetId?: string) {
  return `/tv-review/${encodeURIComponent(projectId)}${reviewTargetId ? `/${encodeURIComponent(reviewTargetId)}` : ""}`;
}

function ProjectPicker({ client, navigate }: { client: BrowserProjectClient; navigate: (path: string) => void }) {
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [error, setError] = useState<ProjectClientError | null>(null);
  useEffect(() => { client.listProjects().then((page) => setProjects(page.items)).catch((reason) => setError(normalizeError(reason))); }, [client]);
  return <section className="workspace-page tv-review-page">
    <div className="page-heading"><div><p className="eyebrow">Voice review / step 1</p><h1>TV review targets</h1></div></div>
    <p className="tv-lede">Choose the project whose verified rough-cut preview you will watch on the TV. A review target binds a recording to one exact revision, movie digest and measured cut mapping.</p>
    {error ? <InlineError error={error} /> : null}
    <div className="project-list">{(projects ?? []).map((project) => <button key={project.projectId} onClick={() => navigate(targetPath(project.projectId))}><span><strong>{project.projectId}</strong><small>{project.state}</small></span><b>Review targets →</b></button>)}</div>
    {projects && !projects.length && !error ? <StatePanel title="No replicated projects"><p>Adopt a desktop project on this server before importing a review target.</p></StatePanel> : null}
  </section>;
}

function readinessBadge(summary: ReviewTargetSummary) {
  if (summary.readiness.ready) return <StatusBadge label="Ready for a session" tone="positive" />;
  if (summary.readiness.freshness !== "fresh") return <StatusBadge label="Verification expired" tone="warning" />;
  return <StatusBadge label="Artifact unavailable" tone="critical" />;
}

function youtubeLabel(summary: ReviewTargetSummary) {
  return summary.youtube.videoId ? `YouTube ${summary.youtube.videoId} · declared, unverified` : "No YouTube link";
}

function ProjectReviewTargets({ client, projectId, navigate }: { client: BrowserProjectClient; projectId: string; navigate: (path: string) => void }) {
  const [targets, setTargets] = useState<ReviewTargetSummary[] | null>(null);
  const [error, setError] = useState<ProjectClientError | null>(null);
  const load = () => client.listReviewTargets(projectId, 50).then((page) => { setTargets(page.items); setError(null); }).catch((reason) => setError(normalizeError(reason)));
  useEffect(() => { void load(); }, [client, projectId]);
  return <section className="workspace-page tv-review-page">
    <div className="page-heading"><div><p className="eyebrow">Voice review / {projectId}</p><h1>Review targets</h1></div><button className="secondary" onClick={() => navigate("/tv-review")}>All projects</button></div>
    {error ? <p className="inline-error" role="alert">{describe(error)}</p> : null}
    <div className="tv-review-columns">
      <div>
        <h2 className="tv-section-title">Imported targets</h2>
        <div className="project-list">{(targets ?? []).map((target) => <button key={target.reviewTargetId} onClick={() => navigate(targetPath(projectId, target.reviewTargetId))}>
          <span><strong>Revision {target.revisionId}</strong><small>{formatSeconds(target.durationSeconds)} · {target.cutCount} cuts · digest {short(target.targetDigest)} · {youtubeLabel(target)}</small></span>
          {readinessBadge(target)}
        </button>)}</div>
        {targets && !targets.length ? <StatePanel title="No review targets yet"><p>Export one on the Mac with <code>rcdvlog edit-plan review-target-export --artifact &lt;preview_id&gt;</code>, then import it here.</p></StatePanel> : null}
      </div>
      <ImportPanel client={client} projectId={projectId} imported={(id) => navigate(targetPath(projectId, id))} />
    </div>
  </section>;
}

function ImportPanel({ client, projectId, imported }: { client: BrowserProjectClient; projectId: string; imported: (reviewTargetId: string) => void }) {
  const [text, setText] = useState("");
  const [issuer, setIssuer] = useState("");
  const [preflight, setPreflight] = useState<ReviewTargetPreflight | null>(null);
  const [parseError, setParseError] = useState<string | null>(null);
  const [error, setError] = useState<ProjectClientError | null>(null);
  const [busy, setBusy] = useState(false);

  function request() {
    let parsed: unknown;
    try { parsed = JSON.parse(text); } catch { setParseError("This is not valid JSON. Paste the export output or choose its .json file."); return null; }
    const handoff = unwrapHandoff(parsed);
    if (!handoff) { setParseError("Expected a review target handoff object."); return null; }
    setParseError(null);
    return { handoff, declaredIssuer: issuer.trim() || null };
  }

  async function check(event?: FormEvent) {
    event?.preventDefault();
    const body = request();
    if (!body) return;
    setBusy(true); setError(null); setPreflight(null);
    try { setPreflight(await client.preflightReviewTarget(projectId, body)); }
    catch (reason) { setError(normalizeError(reason)); }
    finally { setBusy(false); }
  }

  async function importTarget() {
    const body = request();
    if (!body) return;
    setBusy(true); setError(null);
    try { const result = await client.importReviewTarget(projectId, body); imported(result.reviewTarget.reviewTargetId); }
    catch (reason) { setError(normalizeError(reason)); setBusy(false); }
  }

  async function chooseFile(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    if (!file) return;
    if (file.size > 16 * 1024 * 1024 + 64 * 1024) { setParseError("The file exceeds the 16 MiB handoff limit."); return; }
    setText(await file.text());
    setPreflight(null);
  }

  const failing = preflight?.checks.filter((c) => c.status === "fail") ?? [];
  return <form className="tv-import" onSubmit={check} aria-labelledby="tv-import-title">
    <p className="card-index">Explicit handoff</p>
    <h2 id="tv-import-title">Import a review target</h2>
    <p className="help">Only the path-free snapshot crosses this boundary — never the movie, originals, catalog or bundle files.</p>
    <label htmlFor="tv-handoff">Handoff JSON</label>
    <textarea id="tv-handoff" value={text} onChange={(event) => { setText(event.target.value); setPreflight(null); }} rows={8} spellCheck={false} placeholder='{"schemaVersion": 1, "targetDigest": "…", …}' />
    <label className="file-choice">Or choose the exported .json file<input type="file" accept="application/json,.json" onChange={(event) => void chooseFile(event)} /></label>
    <label htmlFor="tv-issuer">Owning host label (optional)</label>
    <input id="tv-issuer" value={issuer} maxLength={64} onChange={(event) => { setIssuer(event.target.value); setPreflight(null); }} placeholder="e.g. Travel MacBook" />
    {parseError ? <p className="inline-error" role="alert">{parseError}</p> : null}
    {error ? <p className="inline-error" role="alert">{describe(error)}{error.requestId ? ` Request ID: ${error.requestId}` : ""}</p> : null}
    <div className="tv-actions"><button disabled={busy || !text.trim()} className="secondary">{busy && !preflight ? "Checking…" : "Check handoff"}</button>
      {preflight?.importable ? <button type="button" disabled={busy} onClick={() => void importTarget()}>{preflight.outcome === "duplicate" ? "Open existing target" : preflight.outcome === "refresh" ? "Record refreshed verification" : "Import target"}</button> : null}
    </div>
    {preflight ? <section className="tv-preflight" aria-label="Preflight result">
      <p><strong>{preflightTitle(preflight)}</strong></p>
      {preflight.preview ? <dl className="tv-facts">
        <dt>Revision</dt><dd>{preflight.preview.revisionId}</dd>
        <dt>Duration</dt><dd>{formatSeconds(preflight.preview.durationSeconds)} · {preflight.preview.cutCount} cuts</dd>
        <dt>Target digest</dt><dd><code>{short(preflight.preview.targetDigest)}</code></dd>
        <dt>Verified</dt><dd>{preflight.preview.verifiedAt}</dd>
        <dt>YouTube</dt><dd>{preflight.preview.youtubeVideoId ? `${preflight.preview.youtubeVideoId} (declared, unverified)` : "None declared"}</dd>
      </dl> : null}
      <ul className="tv-checks">{preflight.checks.map((c) => <li key={c.name} data-status={c.status}><b>{c.status === "pass" ? "✓" : c.status === "warn" ? "!" : "✕"}</b> <span><strong>{c.name}</strong> {c.code && REVIEW_TARGET_CODE_COPY[c.code] ? REVIEW_TARGET_CODE_COPY[c.code] : c.detail}</span></li>)}</ul>
      {failing.length ? <p className="help">Nothing will be imported until every failing check passes.</p> : null}
    </section> : null}
  </form>;
}

function preflightTitle(preflight: ReviewTargetPreflight) {
  switch (preflight.outcome) {
    case "new": return "Ready to import as a new immutable target.";
    case "duplicate": return "Identical target already imported.";
    case "refresh": return "Same target; this import records a newer verification receipt.";
    case "conflict": return "Conflict: this viewed artifact was imported with different contents.";
    default: return "Blocked: this handoff cannot be imported.";
  }
}

function ReviewTargetDetailView({ client, projectId, reviewTargetId, navigate }: { client: BrowserProjectClient; projectId: string; reviewTargetId: string; navigate: (path: string) => void }) {
  const [detail, setDetail] = useState<ReviewTargetDetail | null>(null);
  const [error, setError] = useState<ProjectClientError | null>(null);
  const [link, setLink] = useState("");
  const [linkError, setLinkError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => { setDetail(null); client.getReviewTarget(projectId, reviewTargetId).then(setDetail).catch((reason) => setError(normalizeError(reason))); }, [client, projectId, reviewTargetId]);

  async function declare(event: FormEvent, clear = false) {
    event.preventDefault();
    const id = clear ? null : parseYoutubeVideoId(link);
    if (!clear && !id) { setLinkError("Paste a YouTube watch, youtu.be, shorts or live link, or an 11-character video ID."); return; }
    setLinkError(null); setBusy(true);
    try {
      const result = await client.declareReviewTargetYoutube(projectId, reviewTargetId, id);
      setLink("");
      navigate(targetPath(projectId, result.reviewTarget.reviewTargetId));
    } catch (reason) { setError(normalizeError(reason)); }
    finally { setBusy(false); }
  }

  if (error) return <section className="workspace-page"><p className="inline-error" role="alert">{describe(error)}</p><button className="secondary" onClick={() => navigate(targetPath(projectId))}>Back to targets</button></section>;
  if (!detail) return <section className="workspace-page"><StatePanel title="Loading review target" busy><p>Reading the immutable snapshot…</p></StatePanel></section>;
  const latest = detail.observations[detail.observations.length - 1];
  return <section className="workspace-page tv-review-page">
    <div className="page-heading"><div><p className="eyebrow">Voice review / {projectId} / target</p><h1>Revision {detail.revisionId}</h1></div><div className="tv-heading-actions">{readinessBadge(detail)}<button className="secondary" onClick={() => navigate(targetPath(projectId))}>All targets</button></div></div>
    {!detail.readiness.ready ? <p className="inline-error" role="alert">New recording sessions are blocked: {detail.readiness.blockers.map((code) => REVIEW_TARGET_CODE_COPY[code] ?? code).join(" ")} Existing evidence is retained.</p> : null}
    <div className="tv-review-columns">
      <div>
        <dl className="tv-facts">
          <dt>Duration</dt><dd>{formatSeconds(detail.durationSeconds)} ({detail.movie.videoFrameCount} frames; picture ends at {formatSeconds(detail.mapping.pictureEndSeconds)})</dd>
          <dt>Preview digest</dt><dd><code title={detail.movie.blake3}>{short(detail.movie.blake3)}</code> · {detail.movie.byteLength.toLocaleString()} bytes</dd>
          <dt>Target digest</dt><dd><code title={detail.targetDigest}>{short(detail.targetDigest)}</code></dd>
          <dt>Viewed artifact</dt><dd>{detail.viewedArtifactId}</dd>
          <dt>Cut mapping</dt><dd>{detail.mapping.status === "measuredComplete" ? "Verified measured mapping" : detail.mapping.status} · {detail.cutCount} cuts · picture from decoded output PTS; source ranges declared</dd>
          <dt>Movie clock</dt><dd>origin PTS {detail.movie.originPts}, time base {detail.movie.timeBaseNumerator}/{detail.movie.timeBaseDenominator}</dd>
          <dt>Verification</dt><dd>{detail.latestVerifiedAt} · {detail.readiness.freshness} · artifact {detail.readiness.availabilityState}{latest ? ` (${latest.availabilitySource})` : ""}</dd>
          <dt>Imported</dt><dd>{detail.provenance.importedAt} by {detail.provenance.importer.actorType.replace("_", " ")}{detail.provenance.declaredIssuer ? ` · issuer declared as “${detail.provenance.declaredIssuer}” (unverified)` : " · issuer unverified"}</dd>
          {detail.provenance.derivedFromReviewTargetId ? <><dt>Derived from</dt><dd><a href={targetPath(projectId, detail.provenance.derivedFromReviewTargetId)} onClick={(event) => { event.preventDefault(); navigate(targetPath(projectId, detail.provenance.derivedFromReviewTargetId!)); }}>earlier target without this YouTube link</a></dd></> : null}
        </dl>
        <h2 className="tv-section-title">Measured cuts</h2>
        <table className="tv-cuts">
          <thead><tr><th scope="col">#</th><th scope="col">Movie</th><th scope="col">Operation</th><th scope="col">Declared source</th><th scope="col">Declared audio</th></tr></thead>
          <tbody>{detail.cuts.map((cut) => <tr key={cut.operationId}>
            <td>{cut.position + 1}</td>
            <td>{formatSeconds(cut.movieInSeconds)}–{formatSeconds(cut.movieOutSeconds)}</td>
            <td><code>{cut.operationId}</code></td>
            <td>{cut.assetId} {cut.sourceInSeconds.toFixed(2)}–{cut.sourceOutSeconds.toFixed(2)}s</td>
            <td>{cut.declaredAudio === null ? "linked source" : cut.declaredAudio.mode === "silent" ? "silent" : `independent (${cut.declaredAudio.assetId})`}</td>
          </tr>)}</tbody>
        </table>
      </div>
      <aside className="tv-import">
        <p className="card-index">YouTube on the TV</p>
        <h2>{detail.youtube.videoId ? `Video ${detail.youtube.videoId}` : "No YouTube video declared"}</h2>
        <p className="help">{detail.youtube.videoId ? "Declared by " + (detail.youtube.declaredBy === "export" ? "the owning host at export" : "a server user") + ". Unverified: nothing proves YouTube or the TV plays these exact bytes." : "Optional. Paste the link you will open on the TV; it is never fetched or embedded."}</p>
        <form onSubmit={(event) => void declare(event)}>
          <label htmlFor="tv-youtube">YouTube link or video ID</label>
          <input id="tv-youtube" value={link} onChange={(event) => setLink(event.target.value)} placeholder="https://youtu.be/…" spellCheck={false} />
          {linkError ? <p className="inline-error" role="alert">{linkError}</p> : null}
          <div className="tv-actions"><button disabled={busy || !link.trim()}>{detail.youtube.videoId ? "Declare a different video" : "Declare video"}</button>
            {detail.youtube.videoId ? <button type="button" className="secondary" disabled={busy} onClick={(event) => void declare(event, true)}>Remove link</button> : null}</div>
          <p className="help">Changing the link creates a separate immutable target; this one, and anything recorded against it, is kept.</p>
        </form>
        <h3>What this target does not establish</h3>
        <ul className="tv-limitations">{detail.limitations.map((line) => <li key={line}>{line}</li>)}</ul>
      </aside>
    </div>
  </section>;
}

function short(digest: string) { return `${digest.slice(0, 12)}…`; }
export function formatSeconds(seconds: number) {
  const whole = Math.max(0, seconds);
  const minutes = Math.floor(whole / 60);
  const rest = whole - minutes * 60;
  return `${minutes}:${rest.toFixed(2).padStart(5, "0")}`;
}
