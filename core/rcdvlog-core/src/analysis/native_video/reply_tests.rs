use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn clock() -> NativeVideoClock {
    NativeVideoClock {
        source_offset_seconds: 42.25,
        seconds_scale: 1.0,
        local_valid_end_seconds: 2.0,
        first_representation_pts: 0,
        representation_time_base_numerator: 1,
        representation_time_base_denominator: 60_000,
        precision: "proposal".into(),
    }
}

fn witness(audio_included: bool, transcript_segments: &[(&str, f64, f64)]) -> PayloadWitness {
    PayloadWitness {
        media_fingerprint: "media-test-fingerprint".into(),
        audio_included,
        transcript_segments: transcript_segments
            .iter()
            .map(|(key, start, end)| {
                (
                    (*key).to_string(),
                    TranscriptWitness {
                        local_span: (*start, *end),
                        source_span: MomentSpan {
                            start_ms: ((42.25 + start) * 1000.0) as u32,
                            end_ms: ((42.25 + end) * 1000.0) as u32,
                        },
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
    }
}

fn candidate() -> Value {
    json!({
        "id": "moment-1",
        "score": 0.8,
        "core": {"start": "00:00.250", "end": "00:01.250"},
        "payoff": {"start": "00:00.500", "end": "00:00.750"},
        "setup": null,
        "context": null,
        "allowedRange": {"start": "00:00.250", "end": "00:01.250"},
        "reason": "A visible movement resolves.",
        "event": "Movement",
        "role": "visual event",
        "evidenceRefs": ["nativeVideo"],
        "uncertainty": ["parsed text is advisory only"],
        "dialogueBoundaries": [],
        "longHold": false,
        "speechBasis": "none"
    })
}

fn reply() -> Value {
    json!({"version": 1, "clock": "localVideoZero", "moments": [candidate()], "flags": []})
}

fn parse_value(
    value: &Value,
    clock: &NativeVideoClock,
    witness: &PayloadWitness,
) -> Result<ParsedReply> {
    parse(&value.to_string(), clock, witness)
}

#[test]
fn maps_local_clock_proposals_to_source_millisecond_locators_with_advisory_uncertainty() {
    let parsed = parse_value(&reply(), &clock(), &witness(false, &[])).unwrap();
    assert_eq!(parsed.candidates.len(), 1);
    let moment = &parsed.candidates[0];
    assert_eq!(moment.core.start_ms, 42_500);
    assert_eq!(moment.core.end_ms, 43_500);
    assert!(moment.core.start_ms <= moment.payoff.start_ms);
    assert!(moment.payoff.end_ms <= moment.core.end_ms);
    assert!(!moment.uncertainty.is_empty());
    assert!(
        moment
            .uncertainty
            .iter()
            .any(|line| line.contains("do not prove exact original frames"))
    );
    assert!(
        moment
            .uncertainty
            .iter()
            .all(|line| !line.contains("audible source/context review"))
    );
    assert!(
        moment
            .evidence
            .iter()
            .all(|evidence| evidence.kind == "native_video_locator")
    );

    let mut with_flag = reply();
    with_flag["flags"] = json!([{
        "kind": "uncertain",
        "span": {"start": "00:00.250", "end": "00:01.250"},
        "confidence": 0.5,
        "observation": "Possible change in the view.",
        "uncertainty": "Needs human review.",
        "evidenceRefs": ["nativeVideo"],
        "speechBasis": "none"
    }]);
    let parsed = parse_value(&with_flag, &clock(), &witness(false, &[])).unwrap();
    assert!(
        parsed.flags[0]
            .uncertainty
            .contains("Assistive observation only")
    );
}

#[test]
fn rejects_wrong_clocks_invalid_bounds_evidence_and_partial_candidate_promotion() {
    let base_witness = witness(false, &[]);
    type ReplyMutation = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(&str, ReplyMutation)> = vec![
        ("wrong version", Box::new(|v| v["version"] = json!(2))),
        (
            "wrong clock",
            Box::new(|v| v["clock"] = json!("sourceClock")),
        ),
        (
            "inverted",
            Box::new(|v| v["moments"][0]["core"] = json!({"start":"00:01.250","end":"00:00.250"})),
        ),
        (
            "out of window",
            Box::new(|v| {
                v["moments"][0]["payoff"] = json!({"start":"00:01.500","end":"00:02.500"})
            }),
        ),
        (
            "unknown citation",
            Box::new(|v| v["moments"][0]["evidenceRefs"] = json!(["nativeVideo", "T1"])),
        ),
        (
            "duplicate citation",
            Box::new(|v| v["moments"][0]["evidenceRefs"] = json!(["nativeVideo", "nativeVideo"])),
        ),
        (
            "unsent audio basis",
            Box::new(|v| v["moments"][0]["speechBasis"] = json!("sourceAudio")),
        ),
        (
            "missing audio citation",
            Box::new(|v| {
                v["moments"][0]["speechBasis"] = json!("sourceAudio");
                v["moments"][0]["evidenceRefs"] = json!(["nativeVideo"]);
            }),
        ),
        (
            "dialogue without speech",
            Box::new(|v| {
                v["moments"][0]["role"] = json!("dialogue");
            }),
        ),
        ("fenced JSON", Box::new(|_| {})),
    ];

    for (name, mutate) in cases {
        let mut value = reply();
        mutate(&mut value);
        let body = if name == "fenced JSON" {
            format!("```json\n{}\n```", reply())
        } else {
            value.to_string()
        };
        assert!(parse(&body, &clock(), &base_witness).is_err(), "{name}");
    }

    let mut bad_clock = clock();
    bad_clock.seconds_scale = f64::NAN;
    assert!(parse_value(&reply(), &bad_clock, &base_witness).is_err());
    let mut bad_clock = clock();
    bad_clock.local_valid_end_seconds = f64::INFINITY;
    assert!(parse_value(&reply(), &bad_clock, &base_witness).is_err());

    let mut two_candidates = reply();
    let mut invalid = candidate();
    invalid["payoff"] = json!({"start":"00:01.500","end":"00:02.500"});
    two_candidates["moments"] = json!([candidate(), invalid]);
    assert!(parse_value(&two_candidates, &clock(), &base_witness).is_err());
}

#[test]
fn speech_requires_cited_sent_source_audio_or_a_cited_whole_transcript_segment() {
    let mut audio_reply = reply();
    audio_reply["moments"][0]["speechBasis"] = json!("sourceAudio");
    audio_reply["moments"][0]["evidenceRefs"] = json!(["nativeVideo", "sourceAudio"]);
    audio_reply["moments"][0]["role"] = json!("dialogue");
    audio_reply["moments"][0]["dialogueBoundaries"] = json!([
        {"start":"00:00.500", "end":"00:00.750"}
    ]);
    assert!(parse_value(&audio_reply, &clock(), &witness(true, &[])).is_ok());
    assert!(parse_value(&audio_reply, &clock(), &witness(false, &[])).is_err());

    let transcript_witness = witness(false, &[("T0", 0.25, 1.25)]);
    let mut transcript_reply = reply();
    transcript_reply["moments"][0]["speechBasis"] = json!("acceptedTranscript");
    transcript_reply["moments"][0]["evidenceRefs"] = json!(["nativeVideo", "T0"]);
    transcript_reply["moments"][0]["role"] = json!("dialogue");
    transcript_reply["moments"][0]["dialogueBoundaries"] = json!([
        {"start":"00:00.500", "end":"00:00.750"}
    ]);
    assert!(parse_value(&transcript_reply, &clock(), &transcript_witness).is_ok());
    let full = parse_value(&transcript_reply, &clock(), &transcript_witness).unwrap();
    assert_eq!(
        full.candidates[0].dialogue_boundaries,
        vec![MomentSpan {
            start_ms: 42_500,
            end_ms: 43_500
        }]
    );
    let mut clipped = transcript_reply.clone();
    clipped["moments"][0]["core"] = json!({"start":"00:00.500","end":"00:00.750"});
    assert!(parse_value(&clipped, &clock(), &transcript_witness).is_err());
    clipped["moments"][0]["context"] = json!({"start":"00:00.250","end":"00:01.250"});
    assert_eq!(
        parse_value(&clipped, &clock(), &transcript_witness)
            .unwrap()
            .candidates[0]
            .dialogue_boundaries,
        vec![MomentSpan {
            start_ms: 42_500,
            end_ms: 43_500
        }]
    );

    let mut unsent = transcript_reply.clone();
    unsent["moments"][0]["evidenceRefs"] = json!(["nativeVideo", "T1"]);
    assert!(parse_value(&unsent, &clock(), &transcript_witness).is_err());
    let mut out_of_segment = transcript_reply;
    out_of_segment["moments"][0]["dialogueBoundaries"] = json!([
        {"start":"00:01.250", "end":"00:01.500"}
    ]);
    assert!(parse_value(&out_of_segment, &clock(), &transcript_witness).is_err());

    let schema_without_evidence = schema(&witness(false, &[]));
    let candidate_basis = &schema_without_evidence["properties"]["moments"]["items"]["properties"]
        ["speechBasis"]["enum"];
    let flag_basis = &schema_without_evidence["properties"]["flags"]["items"]["properties"]["speechBasis"]
        ["enum"];
    assert_eq!(candidate_basis, &json!(["none"]));
    assert_eq!(flag_basis, &json!(["none"]));
}

#[test]
fn empty_candidate_and_flag_lists_are_valid() {
    let empty = json!({"version":1,"clock":"localVideoZero","moments":[],"flags":[]});
    let parsed = parse_value(&empty, &clock(), &witness(false, &[])).unwrap();
    assert!(parsed.candidates.is_empty());
    assert!(parsed.flags.is_empty());
}
