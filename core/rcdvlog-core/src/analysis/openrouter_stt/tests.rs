use super::decode::MAX_GENERATION_ID_BYTES;
use super::*;
use hound::WavReader;
use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::thread;

fn provider_policy() -> NativeSttProviderPolicy {
    NativeSttProviderPolicy::new(true, SttDataCollection::Deny)
}

fn request() -> NativeSttRequest {
    NativeSttRequest::new(
        OPENROUTER_STT_PROVIDER,
        OPENROUTER_WHISPER_MODEL,
        Some("en".to_string()),
        0.0,
        provider_policy(),
    )
    .unwrap()
}

fn audio_window() -> NativeSttAudioWindow {
    let pcm = vec![0.25_f32; 32_000];
    let window = pack_windows(&[SpeechRegion {
        start_ms: 100,
        end_ms: 1_100,
    }])
    .remove(0);
    NativeSttAudioWindow::from_pcm(&pcm, &window).unwrap()
}

fn valid_response() -> Value {
    json!({
        "text": "hello world",
        "language": "en",
        "duration": 1.0,
        "segments": [
            {
                "id": 0,
                "start": 0.0,
                "end": 1.0,
                "text": "hello world",
                "confidence": 0.92,
                "avg_logprob": -0.2,
                "unknown_provider_field": "ignored",
                "words": [
                    {"word": "hello", "start": 0.0, "end": 0.4, "confidence": 0.95}
                ]
            }
        ],
        "usage": {
            "cost": 0.0001,
            "seconds": 1.0,
            "input_tokens": 25,
            "output_tokens": 3
        }
    })
}

fn fake_provider(
    response_status: &str,
    response_body: &str,
    response_headers: &[(&str, &str)],
) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let status_line = format!("HTTP/1.1 {response_status}\r\n");
    let body = response_body.to_string();
    let headers = response_headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    let response = format!(
        "{status_line}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{}",
        body.len(),
        body
    );
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut content_length = 0_usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some(value) = line
                .strip_prefix("content-length:")
                .or_else(|| line.strip_prefix("Content-Length:"))
            {
                content_length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0_u8; content_length];
        reader.read_exact(&mut body).unwrap();
        stream.write_all(response.as_bytes()).unwrap();
        stream.flush().unwrap();
        format!("{request_line}{}", String::from_utf8(body).unwrap())
    });
    (
        format!("http://{address}/api/v1/audio/transcriptions"),
        handle,
    )
}

#[test]
fn explicit_registry_accepts_verified_whisper_and_rejects_chat_or_unqualified_models() {
    assert_eq!(
        resolve_native_stt_model(OPENROUTER_STT_PROVIDER, OPENROUTER_WHISPER_MODEL)
            .unwrap()
            .endpoint,
        OPENROUTER_STT_ENDPOINT
    );
    assert!(resolve_native_stt_model(OPENROUTER_STT_PROVIDER, "qwen/qwen3.7-flash").is_err());
    assert!(resolve_native_stt_model(OPENROUTER_STT_PROVIDER, "whisper-1").is_err());
    assert!(resolve_native_stt_model("other", OPENROUTER_WHISPER_MODEL).is_err());
}

#[test]
fn local_offline_selection_resolves_without_cloud_or_model_loading() {
    let resolved = resolve_speech_backend(&SpeechBackendSelection::LocalWhisper).unwrap();
    assert_eq!(
        resolved,
        ResolvedSpeechBackend::LocalWhisper {
            model: WHISPER_MODEL.file_name,
            vad_model: VAD_MODEL.file_name,
        }
    );
    assert!(matches!(
        resolved,
        ResolvedSpeechBackend::LocalWhisper { .. }
    ));
}

#[test]
fn request_requires_explicit_iso_language_and_bounded_temperature() {
    assert!(validate_iso639_1("en").is_ok());
    assert!(validate_iso639_1("EN").is_err());
    assert!(validate_iso639_1("eng").is_err());
    assert!(
        NativeSttRequest::new(
            OPENROUTER_STT_PROVIDER,
            OPENROUTER_WHISPER_MODEL,
            Some("en".into()),
            1.0,
            provider_policy(),
        )
        .is_ok()
    );
    assert!(
        NativeSttRequest::new(
            OPENROUTER_STT_PROVIDER,
            OPENROUTER_WHISPER_MODEL,
            Some("en".into()),
            1.01,
            provider_policy(),
        )
        .is_err()
    );
    assert!(
        NativeSttRequest::new(
            OPENROUTER_STT_PROVIDER,
            OPENROUTER_WHISPER_MODEL,
            Some("en".into()),
            f64::NAN,
            provider_policy(),
        )
        .is_err()
    );
}

#[test]
fn provider_routing_allows_only_data_policy_and_rejects_chat_restrictions() {
    let policy = NativeSttProviderPolicy::from_wire_value(&json!({
        "zdr": true,
        "data_collection": "deny"
    }))
    .unwrap();
    assert_eq!(policy, provider_policy());
    let wire = policy.to_wire_value().unwrap();
    assert_eq!(wire, json!({"zdr": true, "data_collection": "deny"}));

    for unsupported in [
        json!({"order": ["openai"]}),
        json!({"only": ["openai"]}),
        json!({"ignore": ["openai"]}),
        json!({"zdr": true, "max_price": {"request": 0.1}}),
        json!({"data_collection": "deny", "require_parameters": true}),
    ] {
        assert!(NativeSttProviderPolicy::from_wire_value(&unsupported).is_err());
    }
}

#[test]
fn request_body_uses_raw_base64_wav_native_fields_and_no_chat_contract() {
    let request = request();
    let audio = audio_window();
    let body = build_native_stt_request_body(&request, &audio).unwrap();

    assert_eq!(body["model"], OPENROUTER_WHISPER_MODEL);
    assert_eq!(body["input_audio"]["format"], "wav");
    assert_eq!(body["response_format"], "verbose_json");
    assert_eq!(body["timestamp_granularities"], json!(["segment"]));
    assert_eq!(
        body["provider"],
        json!({"zdr": true, "data_collection": "deny"})
    );
    assert!(body.get("messages").is_none());
    assert!(body.get("max_tokens").is_none());
    assert!(body["input_audio"].get("url").is_none());
    assert!(
        !body["input_audio"]["data"]
            .as_str()
            .unwrap()
            .starts_with("data:")
    );

    let encoded = body["input_audio"]["data"].as_str().unwrap();
    let wav_bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let mut reader = WavReader::new(std::io::Cursor::new(wav_bytes)).unwrap();
    assert_eq!(reader.spec().sample_rate, 16_000);
    assert_eq!(reader.spec().channels, 1);
    assert_eq!(reader.spec().bits_per_sample, 16);
    assert_eq!(reader.samples::<i16>().count(), audio.sample_count());
}

#[test]
fn native_quote_reserves_full_registered_window_without_fake_chat_components() {
    let request = request();
    let audio = audio_window();
    assert_eq!(audio.duration_ms(), 1_000);
    let bound = native_stt_request_cost_bound(&request, &audio).unwrap();
    bound.validate().unwrap();

    let audio_bound = bound.audio.as_ref().expect("native audio component");
    assert_eq!(audio_bound.input_duration_ms, 1_000);
    assert_eq!(audio_bound.billable_seconds_max, 30);
    assert!((audio_bound.duration_price_usd_per_second - 0.0001).abs() < f64::EPSILON);
    assert!((bound.maximum_cost_usd - 0.003).abs() < 1e-15);
    assert_eq!(bound.prompt_tokens_max, 0);
    assert_eq!(bound.completion_tokens_max, 0);
    assert_eq!(bound.image_count, 0);
    assert_eq!(bound.prompt_price_usd_per_million, 0.0);
    assert_eq!(bound.completion_price_usd_per_million, 0.0);
    assert_eq!(bound.image_price_usd_each, 0.0);
    assert_eq!(bound.request_price_usd_each, 0.0);
    assert_eq!(bound.contract_version, native_stt_cost_bound_contract_id());
    let assumptions = bound.assumptions.join("\n").to_lowercase();
    assert!(assumptions.contains("does not enforce provider.max_price"));
    assert!(assumptions.contains("external invoice"));
    assert!(assumptions.contains("unknown usage"));

    let body = build_native_stt_request_body(&request, &audio).unwrap();
    assert!(body["provider"].get("max_price").is_none());
}

#[test]
fn native_cost_contract_identity_tracks_exact_endpoint_window_and_tariff() {
    let stable = native_stt_cost_bound_contract_id();
    assert_eq!(stable, native_stt_cost_bound_contract_id());
    assert_eq!(
        stable,
        "openrouter.native-stt-duration-reservation.v1:b6ffe247928304d02731f3d71490d1e5aaded9eceb98267951e3a8b91f6406b8"
    );
    assert!(stable.starts_with(NATIVE_STT_COST_BOUND_CONTRACT_VERSION));

    let mut changed = NATIVE_STT_MODELS.to_vec();
    changed[0].duration_price_micro_usd_per_second += 1;
    assert_ne!(native_stt_cost_bound_contract_id_for(&changed), stable);

    let mut changed = NATIVE_STT_MODELS.to_vec();
    changed[0].max_window_ms += 1;
    assert_ne!(native_stt_cost_bound_contract_id_for(&changed), stable);

    let mut changed = NATIVE_STT_MODELS.to_vec();
    changed[0].endpoint = "https://openrouter.ai/api/v1/audio/transcriptions-v2";
    assert_ne!(native_stt_cost_bound_contract_id_for(&changed), stable);

    let mut changed = NATIVE_STT_MODELS.to_vec();
    changed[0].model = "openai/whisper-2";
    assert_ne!(native_stt_cost_bound_contract_id_for(&changed), stable);
}

#[test]
fn planning_silence_returns_no_bounded_audio_and_makes_no_provider_request() {
    let pcm = vec![0.0_f32; 16_000];
    let mut provider_request_count = 0_u32;
    let windows = plan_native_stt_audio_windows(&pcm, |_| Ok(Vec::new())).unwrap();
    for _ in &windows {
        provider_request_count += 1;
    }
    assert!(windows.is_empty());
    assert_eq!(provider_request_count, 0);
}

#[test]
fn planning_packs_only_bounded_speech_windows_from_supplied_pcm() {
    let pcm = vec![0.1_f32; 16_000 * 50];
    let regions = vec![
        SpeechRegion {
            start_ms: 0,
            end_ms: 20_000,
        },
        SpeechRegion {
            start_ms: 30_000,
            end_ms: 45_000,
        },
    ];
    let windows = plan_native_stt_audio_windows(&pcm, |_| Ok(regions.clone())).unwrap();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].duration_ms(), 20_000);
    assert_eq!(windows[1].duration_ms(), 15_000);
    for window in &windows {
        assert!(window.duration_ms() <= WINDOW_MS);
        validate_wav_16k_mono(window.wav_bytes()).unwrap();
    }
}

#[test]
fn bounded_audio_construction_rejects_out_of_range_windows_and_forged_sizes() {
    let pcm = vec![0.0_f32; 160];
    let window = PackedWindow {
        regions: vec![crate::analysis::windows::PackedRegion {
            window_offset_ms: 0,
            source_start_ms: 5,
            duration_ms: 100,
        }],
    };
    assert!(NativeSttAudioWindow::from_pcm(&pcm, &window).is_err());
    assert!(seconds_to_window_ms(f64::NAN, 30_000).is_err());
    assert!(seconds_to_window_ms(f64::INFINITY, 30_000).is_err());
    assert!(seconds_to_window_ms(-0.1, 30_000).is_err());
    assert!(seconds_to_window_ms(30.1, 30_000).is_err());
}

#[test]
fn valid_verbose_response_keeps_optional_quality_evidence_honest() {
    let transcript = decode_native_stt_value(&valid_response(), 1_000).unwrap();
    assert_eq!(transcript.text, "hello world");
    assert_eq!(transcript.language, "en");
    assert_eq!(transcript.duration_ms, 1_000);
    assert_eq!(transcript.segments.len(), 1);
    assert_eq!(transcript.segments[0].confidence, Some(0.92));
    assert_eq!(transcript.segments[0].avg_logprob, Some(-0.2));
    assert_eq!(transcript.segments[0].words[0].confidence, Some(0.95));

    let mut missing = valid_response();
    missing["segments"][0]
        .as_object_mut()
        .unwrap()
        .remove("confidence");
    missing["segments"][0]
        .as_object_mut()
        .unwrap()
        .remove("avg_logprob");
    let transcript = decode_native_stt_value(&missing, 1_000).unwrap();
    assert_eq!(transcript.segments[0].confidence, None);
    assert_eq!(transcript.segments[0].avg_logprob, None);

    let mut missing_word_confidence = valid_response();
    missing_word_confidence["segments"][0]["words"][0]
        .as_object_mut()
        .unwrap()
        .remove("confidence");
    let transcript = decode_native_stt_value(&missing_word_confidence, 1_000).unwrap();
    assert_eq!(transcript.segments[0].words[0].confidence, None);

    let mut invalid_confidence = valid_response();
    invalid_confidence["segments"][0]["confidence"] = json!(1.2);
    assert_eq!(
        decode_native_stt_value(&invalid_confidence, 1_000).unwrap_err(),
        NativeSttProtocolError::InvalidQualityEvidence
    );

    let mut invalid_logprob = valid_response();
    invalid_logprob["segments"][0]["avg_logprob"] = json!(0.1);
    assert_eq!(
        decode_native_stt_value(&invalid_logprob, 1_000).unwrap_err(),
        NativeSttProtocolError::InvalidQualityEvidence
    );
}

#[test]
fn missing_required_response_shape_is_typed_unusable() {
    let mut missing_text = valid_response();
    missing_text.as_object_mut().unwrap().remove("text");
    assert_eq!(
        decode_native_stt_value(&missing_text, 1_000).unwrap_err(),
        NativeSttProtocolError::MissingText
    );

    let mut missing_language = valid_response();
    missing_language.as_object_mut().unwrap().remove("language");
    assert_eq!(
        decode_native_stt_value(&missing_language, 1_000).unwrap_err(),
        NativeSttProtocolError::MissingLanguage
    );

    let mut missing_duration = valid_response();
    missing_duration.as_object_mut().unwrap().remove("duration");
    assert_eq!(
        decode_native_stt_value(&missing_duration, 1_000).unwrap_err(),
        NativeSttProtocolError::MissingDuration
    );

    let mut missing_segments = valid_response();
    missing_segments.as_object_mut().unwrap().remove("segments");
    assert_eq!(
        decode_native_stt_value(&missing_segments, 1_000).unwrap_err(),
        NativeSttProtocolError::MissingSegments
    );
}

#[test]
fn timestamp_violations_are_rejected_in_window_time() {
    let mut out_of_window = valid_response();
    out_of_window["segments"][0]["start"] = json!(0.0);
    out_of_window["segments"][0]["end"] = json!(1.1);
    assert_eq!(
        decode_native_stt_value(&out_of_window, 1_000).unwrap_err(),
        NativeSttProtocolError::InvalidTimestamp
    );

    let mut reversed = valid_response();
    reversed["segments"][0]["start"] = json!(0.8);
    reversed["segments"][0]["end"] = json!(0.2);
    assert_eq!(
        decode_native_stt_value(&reversed, 1_000).unwrap_err(),
        NativeSttProtocolError::InvalidTimestamp
    );

    let mut out_of_order = valid_response();
    out_of_order["segments"] = json!([
        {"start": 0.5, "end": 0.8, "text": "second"},
        {"start": 0.1, "end": 0.4, "text": "first"}
    ]);
    assert_eq!(
        decode_native_stt_value(&out_of_order, 1_000).unwrap_err(),
        NativeSttProtocolError::OutOfOrderTimestamp
    );

    let mut missing_start = valid_response();
    missing_start["segments"][0]
        .as_object_mut()
        .unwrap()
        .remove("start");
    assert_eq!(
        decode_native_stt_value(&missing_start, 1_000).unwrap_err(),
        NativeSttProtocolError::MissingTimestamp
    );

    let mut negative = valid_response();
    negative["segments"][0]["start"] = json!(-0.1);
    assert_eq!(
        decode_native_stt_value(&negative, 1_000).unwrap_err(),
        NativeSttProtocolError::InvalidTimestamp
    );
}

#[test]
fn malformed_paid_response_retains_receipt_for_later_recovery() {
    let payload = json!({
        "text": "hello",
        "language": "en",
        "duration": 1.0,
        "usage": {"cost": 0.04, "seconds": 1.0}
    });
    let body = serde_json::to_vec(&payload).unwrap();
    let result = decode_native_stt_body(&body, 1_000, Some("gen-recovery".into()), 17);
    assert_eq!(
        result.outcome,
        NativeSttOutcome::Unusable(NativeSttProtocolError::MissingSegments)
    );
    assert_eq!(
        result.receipt.generation_id.as_deref(),
        Some("gen-recovery")
    );
    assert_eq!(result.receipt.usage.cost, NativeSttCost::Known(0.04));
    assert_eq!(result.latency_ms, 17);

    let malformed = decode_native_stt_body(b"{not json", 1_000, Some("gen-malformed".into()), 3);
    assert_eq!(
        malformed.outcome,
        NativeSttOutcome::Unusable(NativeSttProtocolError::InvalidJson)
    );
    assert_eq!(
        malformed.receipt.generation_id.as_deref(),
        Some("gen-malformed")
    );
    assert_eq!(malformed.receipt.usage.cost, NativeSttCost::Unknown);

    let malformed_text = json!({
        "text": 7,
        "language": "en",
        "duration": 1.0,
        "segments": [{"start": 0.0, "end": 1.0, "text": "hello"}],
        "usage": {"cost": 0.05}
    });
    let malformed_text = decode_native_stt_body(
        &serde_json::to_vec(&malformed_text).unwrap(),
        1_000,
        Some("gen-malformed-text".into()),
        4,
    );
    assert_eq!(
        malformed_text.outcome,
        NativeSttOutcome::Unusable(NativeSttProtocolError::InvalidText)
    );
    assert_eq!(
        malformed_text.receipt.generation_id.as_deref(),
        Some("gen-malformed-text")
    );
    assert_eq!(
        malformed_text.receipt.usage.cost,
        NativeSttCost::Known(0.05)
    );

    let malformed_segment = json!({
        "text": "hello",
        "language": "en",
        "duration": 1.0,
        "segments": [{"start": 0.0, "end": 1.0, "text": false}],
        "usage": {"cost": 0.06}
    });
    let malformed_segment = decode_native_stt_body(
        &serde_json::to_vec(&malformed_segment).unwrap(),
        1_000,
        Some("gen-malformed-segment".into()),
        5,
    );
    assert_eq!(
        malformed_segment.outcome,
        NativeSttOutcome::Unusable(NativeSttProtocolError::InvalidText)
    );
    assert_eq!(
        malformed_segment.receipt.generation_id.as_deref(),
        Some("gen-malformed-segment")
    );
    assert_eq!(
        malformed_segment.receipt.usage.cost,
        NativeSttCost::Known(0.06)
    );
}

#[test]
fn usage_cost_is_known_only_when_valid_and_unknown_otherwise() {
    let parsed = parse_native_stt_receipt(
        &json!({
            "model": "actual/model",
            "provider": "Actual Provider",
            "usage": {"cost": 0.02, "seconds": 0.5, "input_tokens": 7, "output_tokens": 2}
        }),
        Some("gen-1".into()),
    );
    assert_eq!(parsed.actual_model.as_deref(), Some("actual/model"));
    assert_eq!(parsed.actual_provider.as_deref(), Some("Actual Provider"));
    assert_eq!(parsed.usage.cost, NativeSttCost::Known(0.02));
    assert_eq!(parsed.usage.seconds, Some(0.5));
    assert_eq!(parsed.usage.input_tokens, Some(7));
    assert_eq!(parsed.usage.output_tokens, Some(2));

    let missing = parse_native_stt_receipt(&json!({}), None);
    assert_eq!(missing.actual_model, None);
    assert_eq!(missing.actual_provider, None);
    assert_eq!(missing.usage.cost, NativeSttCost::Unknown);
    assert_eq!(missing.usage.seconds, None);

    let malformed = parse_native_stt_receipt(
        &json!({"usage": {"cost": "0.02", "seconds": -1, "input_tokens": -2}}),
        Some("".into()),
    );
    assert_eq!(malformed.usage.cost, NativeSttCost::Unknown);
    assert_eq!(malformed.usage.seconds, None);
    assert_eq!(malformed.usage.input_tokens, None);
    assert_eq!(malformed.generation_id, None);

    let oversized_id = "x".repeat(MAX_GENERATION_ID_BYTES + 1);
    assert_eq!(
        parse_native_stt_receipt(&json!({}), Some(oversized_id)).generation_id,
        None
    );
}

#[test]
fn client_sends_exact_native_path_and_parses_provider_receipt_and_latency() {
    let response_body = serde_json::to_string(&valid_response()).unwrap();
    let (endpoint, server) = fake_provider(
        "200 OK",
        &response_body,
        &[("X-Generation-Id", "gen-http-1")],
    );
    let client = OpenRouterSttClient::new("test-key")
        .unwrap()
        .with_endpoint(endpoint);
    let result = client.transcribe(&request(), &audio_window()).unwrap();
    let captured = server.join().unwrap();
    assert!(captured.starts_with("POST /api/v1/audio/transcriptions HTTP/1.1"));
    assert!(!captured.contains("chat/completions"));
    assert!(matches!(result.outcome, NativeSttOutcome::Usable(_)));
    assert_eq!(result.receipt.generation_id.as_deref(), Some("gen-http-1"));
    assert_eq!(result.receipt.usage.cost, NativeSttCost::Known(0.0001));
}

#[test]
fn provider_error_body_is_not_exposed_in_safe_client_error() {
    let (endpoint, server) = fake_provider(
        "500 Internal Server Error",
        "PRIVATE_PROVIDER_RESPONSE_CANARY",
        &[],
    );
    let client = OpenRouterSttClient::new("test-key")
        .unwrap()
        .with_endpoint(endpoint);
    let result = client.transcribe(&request(), &audio_window()).unwrap();
    let NativeSttOutcome::Unusable(error) = result.outcome else {
        panic!("HTTP 500 accepted");
    };
    let safe = error.to_string();
    server.join().unwrap();
    assert!(
        safe.contains("native_stt_protocol:http_status:500"),
        "{safe}"
    );
    assert!(!safe.contains("PRIVATE_PROVIDER_RESPONSE_CANARY"), "{safe}");
}

#[test]
fn cancellation_and_missing_credentials_fail_locally_without_upload() {
    let error = OpenRouterSttClient::new("  ").unwrap_err();
    assert!(format!("{error:#}").contains("native_stt_client:configuration"));

    let client = OpenRouterSttClient::new("test-key")
        .unwrap()
        .with_endpoint("http://127.0.0.1:9/unreachable");
    let error = client
        .transcribe_with_cancel(&request(), &audio_window(), &mut || true)
        .unwrap_err();
    assert!(format!("{error:#}").contains("native_stt_client:cancelled_before_upload"));
}

#[test]
fn missing_usage_and_returned_identity_are_not_fabricated_by_http_decode() {
    let response_body = json!({
        "text": "hello",
        "language": "en",
        "duration": 1.0,
        "segments": [{"start": 0.0, "end": 1.0, "text": "hello"}]
    })
    .to_string();
    let (endpoint, server) = fake_provider(
        "200 OK",
        &response_body,
        &[("X-Generation-Id", "gen-no-usage")],
    );
    let client = OpenRouterSttClient::new("test-key")
        .unwrap()
        .with_endpoint(endpoint);
    let result = client.transcribe(&request(), &audio_window()).unwrap();
    server.join().unwrap();
    assert!(matches!(result.outcome, NativeSttOutcome::Usable(_)));
    assert_eq!(result.receipt.actual_model, None);
    assert_eq!(result.receipt.actual_provider, None);
    assert_eq!(result.receipt.usage.cost, NativeSttCost::Unknown);
    assert_eq!(result.receipt.usage.seconds, None);
}
#[test]
fn http_error_retains_paid_receipt_without_echoing_body() {
    let body = json!({"usage": {"cost": 0.04}, "error": "PRIVATE_ERROR_CANARY"}).to_string();
    let (endpoint, server) = fake_provider(
        "503 Service Unavailable",
        &body,
        &[("X-Generation-Id", "gen-error")],
    );
    let client = OpenRouterSttClient::new("test-key")
        .unwrap()
        .with_endpoint(endpoint);
    let result = client.transcribe(&request(), &audio_window()).unwrap();
    server.join().unwrap();
    assert_eq!(result.receipt.generation_id.as_deref(), Some("gen-error"));
    assert_eq!(result.receipt.usage.cost, NativeSttCost::Known(0.04));
    assert_eq!(
        result.outcome,
        NativeSttOutcome::Unusable(NativeSttProtocolError::HttpStatus(503))
    );
    assert!(!format!("{:?}", result).contains("PRIVATE_ERROR_CANARY"));
}

#[test]
fn redirect_never_retransmits_audio_to_another_endpoint() {
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    destination.set_nonblocking(true).unwrap();
    let location = format!("http://{}/private", destination.local_addr().unwrap());
    let (endpoint, server) =
        fake_provider("307 Temporary Redirect", "", &[("Location", &location)]);
    let client = OpenRouterSttClient::new("test-key")
        .unwrap()
        .with_endpoint(endpoint);
    let result = client.transcribe(&request(), &audio_window()).unwrap();
    server.join().unwrap();
    assert_eq!(
        result.outcome,
        NativeSttOutcome::Unusable(NativeSttProtocolError::HttpStatus(307))
    );
    assert_eq!(
        destination.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn invalid_vad_ranges_do_not_turn_into_silence_or_duplicated_audio() {
    let pcm = vec![0.1; 16_000];
    for regions in [
        vec![SpeechRegion {
            start_ms: 500,
            end_ms: 100,
        }],
        vec![
            SpeechRegion {
                start_ms: 0,
                end_ms: 500,
            },
            SpeechRegion {
                start_ms: 300,
                end_ms: 700,
            },
        ],
        vec![SpeechRegion {
            start_ms: 0,
            end_ms: 1_001,
        }],
    ] {
        assert!(plan_native_stt_audio_windows(&pcm, |_| Ok(regions.clone())).is_err());
    }
}

#[test]
fn exact_source_bound_and_response_size_are_enforced_before_decode() {
    assert!(seconds_to_window_ms(1.0001, 1_000).is_err());
    let oversized = vec![b' '; MAX_NATIVE_STT_RESPONSE_BYTES + 1];
    let result = decode_native_stt_body(&oversized, 1_000, Some("gen-bounded".into()), 3);
    assert_eq!(
        result.outcome,
        NativeSttOutcome::Unusable(NativeSttProtocolError::ResponseTooLarge)
    );
    assert_eq!(result.receipt.generation_id.as_deref(), Some("gen-bounded"));
    let mut contradictory = valid_response();
    contradictory["duration"] = json!(0.5);
    assert!(decode_native_stt_value(&contradictory, 1_000).is_err());
    assert!(validate_iso639_1("zz").is_err());
    assert!(
        parse_native_stt_receipt(&json!({}), Some(" gen-padded ".into()))
            .generation_id
            .is_none()
    );
    let audio = audio_window();
    assert!(!format!("{audio:?}").contains("wav_bytes"));
}

#[test]
fn provider_language_and_full_confidence_are_validated_without_invention() {
    let mut payload = valid_response();
    payload["language"] = json!("english");
    payload["confidence"] = json!(0.12);
    let decoded = decode_native_stt_value(&payload, 1_000).unwrap();
    assert_eq!(decoded.language, "english");
    assert_eq!(decoded.confidence, Some(0.12));
    payload["language"] = json!("PRIVATE_LANGUAGE_CANARY");
    let error = decode_native_stt_value(&payload, 1_000).unwrap_err();
    assert_eq!(error, NativeSttProtocolError::InvalidLanguage);
    assert!(!error.to_string().contains("PRIVATE_LANGUAGE_CANARY"));
    payload["language"] = json!("en");
    payload["confidence"] = json!(2.0);
    assert_eq!(
        decode_native_stt_value(&payload, 1_000).unwrap_err(),
        NativeSttProtocolError::InvalidQualityEvidence
    );
}
