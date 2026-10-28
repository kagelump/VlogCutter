//! Strict local-clock replies. Raw rejected text stays with the cache owner.
use super::clock::parse_local_clock;
use super::{NativeVideoClock, NativeVideoFlag};
use crate::moments::{MomentCandidate, MomentEvidenceRef, MomentSpan};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub(super) struct PayloadWitness {
    pub media_fingerprint: String,
    pub audio_included: bool,
    /// Only whole accepted segments actually attached to this request.
    pub transcript_segments: BTreeMap<String, TranscriptWitness>,
}
#[derive(Clone, Copy)]
pub(super) struct TranscriptWitness {
    pub local_span: (f64, f64),
    pub source_span: MomentSpan,
}
impl PayloadWitness {
    pub fn keys(&self) -> BTreeSet<String> {
        let mut keys = BTreeSet::from(["nativeVideo".into()]);
        if self.audio_included {
            keys.insert("sourceAudio".into());
        }
        keys.extend(self.transcript_segments.keys().cloned());
        keys
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Span {
    start: String,
    end: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum SpeechBasis {
    None,
    SourceAudio,
    AcceptedTranscript,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Candidate {
    id: String,
    score: f64,
    core: Span,
    payoff: Span,
    setup: Option<Span>,
    context: Option<Span>,
    allowed_range: Span,
    reason: String,
    event: String,
    role: String,
    evidence_refs: Vec<String>,
    uncertainty: Vec<String>,
    dialogue_boundaries: Vec<Span>,
    long_hold: bool,
    speech_basis: SpeechBasis,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Flag {
    kind: String,
    span: Span,
    confidence: f64,
    observation: String,
    uncertainty: String,
    evidence_refs: Vec<String>,
    speech_basis: SpeechBasis,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reply {
    version: u32,
    clock: String,
    moments: Vec<Candidate>,
    flags: Vec<Flag>,
}
pub(super) struct ParsedReply {
    pub candidates: Vec<MomentCandidate>,
    pub flags: Vec<NativeVideoFlag>,
}
fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max
}
fn local(span: &Span, clock: &NativeVideoClock) -> Result<(f64, f64)> {
    let a = parse_local_clock(&span.start)?;
    let b = parse_local_clock(&span.end)?;
    ensure!(
        a < b && b <= clock.local_valid_end_seconds + 1e-9,
        "native reply interval is empty, inverted or outside the supplied local window"
    );
    Ok((a, b))
}
fn contains(outer: (f64, f64), inner: (f64, f64)) -> bool {
    outer.0 <= inner.0 + 1e-9 && inner.1 <= outer.1 + 1e-9
}
fn mapped(interval: (f64, f64), clock: &NativeVideoClock) -> Result<MomentSpan> {
    let start = (clock.source_offset_seconds + interval.0 * clock.seconds_scale) * 1000.0;
    let end = (clock.source_offset_seconds + interval.1 * clock.seconds_scale) * 1000.0;
    ensure!(
        start.is_finite() && end.is_finite() && start >= 0.0 && end <= f64::from(u32::MAX),
        "native source clock cannot be represented as source milliseconds"
    );
    // The moment owner is a millisecond locator schema, not a frame-PTS schema.
    // Round inward, never silently export evidence beyond the supplied window.
    let span = MomentSpan {
        start_ms: start.ceil() as u32,
        end_ms: end.floor() as u32,
    };
    ensure!(
        span.start_ms < span.end_ms,
        "native interval is too small for the moment locator clock"
    );
    Ok(span)
}
fn check_evidence(
    refs: &[String],
    basis: SpeechBasis,
    witness: &PayloadWitness,
    intervals: &[(f64, f64)],
) -> Result<()> {
    let allowed = witness.keys();
    let mut unique = BTreeSet::new();
    ensure!(
        !refs.is_empty()
            && refs.len() <= 64
            && refs
                .iter()
                .all(|key| allowed.contains(key) && unique.insert(key)),
        "native reply cites missing, duplicate or unsent evidence"
    );
    match basis {
        SpeechBasis::None => ensure!(
            intervals.is_empty(),
            "speech boundaries require actual sent audio or accepted transcript evidence"
        ),
        SpeechBasis::SourceAudio => ensure!(
            witness.audio_included && refs.iter().any(|k| k == "sourceAudio"),
            "native speech claim requires explicit sent source audio citation"
        ),
        SpeechBasis::AcceptedTranscript => {
            let cited = refs
                .iter()
                .filter_map(|key| witness.transcript_segments.get(key))
                .map(|segment| segment.local_span)
                .collect::<Vec<_>>();
            ensure!(
                !cited.is_empty()
                    && intervals
                        .iter()
                        .all(|span| cited.iter().any(|range| contains(*range, *span))),
                "native speech claim exceeds actually supplied whole transcript segments"
            );
        }
    }
    Ok(())
}
pub(super) fn parse(
    body: &str,
    clock: &NativeVideoClock,
    witness: &PayloadWitness,
) -> Result<ParsedReply> {
    ensure!(
        body.len() <= 2 * 1024 * 1024,
        "native reply exceeds bounded response size"
    );
    ensure!(
        clock.source_offset_seconds.is_finite()
            && clock.source_offset_seconds >= 0.0
            && clock.seconds_scale.is_finite()
            && clock.seconds_scale > 0.0
            && clock.local_valid_end_seconds.is_finite()
            && clock.local_valid_end_seconds > 0.0,
        "invalid native source/local clock"
    );
    let reply: Reply = serde_json::from_str(body)
        .context("native response does not match the bounded JSON schema")?;
    ensure!(
        reply.version == 1
            && reply.clock == "localVideoZero"
            && reply.moments.len() <= 64
            && reply.flags.len() <= 128,
        "native response version/clock/count mismatch"
    );
    let mut ids = BTreeSet::new();
    let mut candidates = Vec::new();
    for (index, c) in reply.moments.into_iter().enumerate() {
        ensure!(
            text(&c.id, 64)
                && ids.insert(c.id.clone())
                && c.score.is_finite()
                && (0.0..=1.0).contains(&c.score)
                && text(&c.reason, 4096)
                && text(&c.event, 255)
                && text(&c.role, 255)
                && c.uncertainty.len() <= 32
                && c.uncertainty.iter().all(|s| s.len() <= 4096)
                && c.dialogue_boundaries.len() <= 128,
            "native candidate identity/score/text bound invalid"
        );
        let core = local(&c.core, clock)?;
        let payoff = local(&c.payoff, clock)?;
        let allowance = local(&c.allowed_range, clock)?;
        ensure!(
            contains(allowance, core) && contains(core, payoff),
            "native core/payoff exceed supplied bounds"
        );
        let setup = c.setup.as_ref().map(|s| local(s, clock)).transpose()?;
        let context = c.context.as_ref().map(|s| local(s, clock)).transpose()?;
        let dialogue = c
            .dialogue_boundaries
            .iter()
            .map(|s| local(s, clock))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            setup
                .iter()
                .chain(context.iter())
                .chain(dialogue.iter())
                .all(|span| contains(allowance, *span)),
            "native setup/context/dialogue exceeds candidate allowed range"
        );
        check_evidence(&c.evidence_refs, c.speech_basis, witness, &dialogue)?;
        let whole_speech = c
            .evidence_refs
            .iter()
            .filter_map(|key| witness.transcript_segments.get(key))
            .collect::<Vec<_>>();
        if c.speech_basis != SpeechBasis::None {
            ensure!(
                whole_speech
                    .iter()
                    .all(|segment| contains(core, segment.local_span)
                        || context.is_some_and(|span| contains(span, segment.local_span))),
                "native speech claim must preserve each cited whole accepted utterance in core or explicit context"
            );
        }
        if matches!(
            c.role.trim().to_lowercase().as_str(),
            "dialogue" | "speech" | "narration" | "interview"
        ) {
            ensure!(
                c.speech_basis != SpeechBasis::None,
                "native dialogue role has no permitted speech evidence"
            );
        }
        let mut uncertainty = c.uncertainty;
        uncertainty.push("Native video/utterance times are mapped locator proposals, rounded inward to source milliseconds. They do not prove exact original frames, complete words or editorial quality.".into());
        if c.speech_basis != SpeechBasis::None {
            uncertainty.push("Speech needs audible source/context review; no word alignment or human acceptance is implied.".into());
            if !whole_speech.is_empty() {
                uncertainty.push("Speech holds retain whole supplied accepted utterances; model sub-word times remain unverified.".into());
            }
        }
        let evidence = c
            .evidence_refs
            .iter()
            .map(|key| MomentEvidenceRef {
                kind: match key.as_str() {
                    "nativeVideo" => "native_video_locator",
                    "sourceAudio" => "explicit_source_audio",
                    _ => "accepted_transcript_segment",
                }
                .into(),
                key: format!("{}:{key}", witness.media_fingerprint),
                span: witness
                    .transcript_segments
                    .get(key)
                    .map(|segment| segment.source_span),
            })
            .collect();
        let mut speech_spans = if c.speech_basis == SpeechBasis::AcceptedTranscript {
            vec![]
        } else {
            dialogue
                .into_iter()
                .map(|s| mapped(s, clock))
                .collect::<Result<Vec<_>>>()?
        };
        if c.speech_basis != SpeechBasis::None {
            speech_spans.extend(whole_speech.into_iter().map(|segment| segment.source_span));
        }
        speech_spans.sort_by_key(|s| (s.start_ms, s.end_ms));
        speech_spans.dedup();
        ensure!(
            speech_spans.len() <= 128,
            "native whole-utterance evidence exceeds bounded candidate speech count"
        );
        let core = mapped(core, clock)?;
        let payoff = mapped(payoff, clock)?;
        let allowed_range = mapped(allowance, clock)?;
        let setup = setup.map(|s| mapped(s, clock)).transpose()?;
        let context = context.map(|s| mapped(s, clock)).transpose()?;
        ensure!(
            allowed_range.contains(core)
                && core.contains(payoff)
                && setup
                    .iter()
                    .chain(context.iter())
                    .chain(speech_spans.iter())
                    .all(|&span| allowed_range.contains(span)),
            "native source-millisecond rounding cannot preserve the supplied candidate relationships"
        );
        if c.speech_basis != SpeechBasis::None {
            ensure!(
                c.evidence_refs
                    .iter()
                    .filter_map(|key| witness.transcript_segments.get(key))
                    .all(|s| core.contains(s.source_span)
                        || context.is_some_and(|span| span.contains(s.source_span))),
                "native source-millisecond core/context must preserve each supplied whole utterance"
            );
        }
        candidates.push(MomentCandidate {
            id: c.id,
            rank: (index + 1) as u32,
            score: c.score,
            core,
            payoff,
            setup,
            context,
            allowed_range,
            reason: c.reason,
            event: c.event,
            role: c.role,
            evidence,
            uncertainty,
            dialogue_boundaries: speech_spans,
            refined: false,
            long_hold: c.long_hold,
        });
    }
    let mut flags = Vec::new();
    for flag in reply.flags {
        ensure!(
            matches!(
                flag.kind.as_str(),
                "coveredRange"
                    | "orientationQuestion"
                    | "incompleteAction"
                    | "incompleteSpeech"
                    | "meaningfulDark"
                    | "uncertain"
            ) && flag.confidence.is_finite()
                && (0.0..=1.0).contains(&flag.confidence)
                && text(&flag.observation, 4096)
                && text(&flag.uncertainty, 4096),
            "native review flag kind/score/text invalid"
        );
        let span = local(&flag.span, clock)?;
        if flag.kind == "incompleteSpeech" {
            ensure!(
                flag.speech_basis != SpeechBasis::None,
                "native speech flag requires permitted speech evidence"
            );
        }
        check_evidence(
            &flag.evidence_refs,
            flag.speech_basis,
            witness,
            if flag.speech_basis == SpeechBasis::None {
                &[]
            } else {
                std::slice::from_ref(&span)
            },
        )?;
        flags.push(NativeVideoFlag{kind:flag.kind,source_in_seconds:clock.source_offset_seconds+span.0*clock.seconds_scale,source_out_seconds:clock.source_offset_seconds+span.1*clock.seconds_scale,
            confidence:flag.confidence,observation:flag.observation,uncertainty:format!("{} Assistive observation only; no catalog removal, rotation or human source-range decision was applied.",flag.uncertainty)});
    }
    Ok(ParsedReply { candidates, flags })
}

pub(super) fn schema(witness: &PayloadWitness) -> Value {
    let string = || json!({"type":"string"});
    let number = || json!({"type":"number"});
    let span = json!({"type":"object","additionalProperties":false,"properties":{"start":string(),"end":string()},"required":["start","end"]});
    let refs = json!({"type":"array","items":{"type":"string","enum":witness.keys().into_iter().collect::<Vec<_>>()}});
    let mut speech_bases = vec!["none"];
    if witness.audio_included {
        speech_bases.push("sourceAudio");
    }
    if !witness.transcript_segments.is_empty() {
        speech_bases.push("acceptedTranscript");
    }
    let speech = json!({"type":"string","enum":speech_bases});
    let nullable_span = json!({"anyOf":[span.clone(),{"type":"null"}]});
    let candidate = json!({"type":"object","additionalProperties":false,"properties":{"id":string(),"score":number(),"core":span,"payoff":span,"setup":nullable_span,"context":nullable_span,
        "allowedRange":span,"reason":string(),"event":string(),"role":string(),"evidenceRefs":refs,"uncertainty":{"type":"array","items":string()},
        "dialogueBoundaries":{"type":"array","items":span},"longHold":{"type":"boolean"},"speechBasis":speech},
        "required":["id","score","core","payoff","setup","context","allowedRange","reason","event","role","evidenceRefs","uncertainty","dialogueBoundaries","longHold","speechBasis"]});
    let flag = json!({"type":"object","additionalProperties":false,"properties":{"kind":{"type":"string","enum":["coveredRange","orientationQuestion","incompleteAction","incompleteSpeech","meaningfulDark","uncertain"]},
        "span":span,"confidence":number(),"observation":string(),"uncertainty":string(),"evidenceRefs":refs,"speechBasis":speech},"required":["kind","span","confidence","observation","uncertainty","evidenceRefs","speechBasis"]});
    json!({"type":"object","additionalProperties":false,"properties":{"version":{"type":"integer"},"clock":{"type":"string","enum":["localVideoZero"]},"moments":{"type":"array","items":candidate},"flags":{"type":"array","items":flag}},"required":["version","clock","moments","flags"]})
}

#[cfg(test)]
#[path = "reply_tests.rs"]
mod tests;
