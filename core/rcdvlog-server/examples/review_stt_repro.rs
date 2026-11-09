//! Explicit one-request reproduction of native review STT, with private local receipts.
//! Usage: cargo run -p rcdvlog-server-core --example review_stt_repro -- INPUT.wav MODEL NEW_OUTPUT_DIR
use anyhow::{Context, Result, ensure};
use rcdvlog_core::review_speech::{NativeSttRequest, pack_windows, resolve_api_key};
use rcdvlog_core::{build_native_stt_request_body, decode_native_stt_body};
use serde_json::json;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
use std::{
    io::Read,
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(args.len() == 3, "usage: INPUT.wav MODEL NEW_OUTPUT_DIR");
    let input = PathBuf::from(&args[0]);
    let model = args[1].to_str().context("model ID")?;
    let out = PathBuf::from(&args[2]);
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    builder
        .create(&out)
        .context("create new private diagnostic directory")?;
    let mut wav = hound::WavReader::open(input)?;
    let spec = wav.spec();
    ensure!(
        spec.bits_per_sample == 16 && spec.sample_format == hound::SampleFormat::Int,
        "requires PCM16 WAV"
    );
    let samples = wav.samples::<i16>().collect::<Result<Vec<_>, _>>()?;
    let mono: Vec<f32> = samples
        .chunks_exact(spec.channels as usize)
        .map(|frame| frame.iter().map(|s| *s as f32 / 32768.0).sum::<f32>() / spec.channels as f32)
        .collect();
    let pcm = rcdvlog_server_core::resample_to(&mono, spec.sample_rate, 16000);
    let dir =
        std::env::var_os("RCDVLOG_SPEECH_MODEL_DIR").context("set RCDVLOG_SPEECH_MODEL_DIR")?;
    let regions = rcdvlog_core::review_speech::detect_silero_speech(
        &PathBuf::from(dir).join("ggml-silero-v5.1.2.bin"),
        4,
        &pcm,
    )?;
    let windows = pack_windows(&regions);
    ensure!(
        windows.len() == 1,
        "repro requires exactly one speech window; got {}",
        windows.len()
    );
    let request = NativeSttRequest::for_discovered_model(model.into(), None)?;
    let audio = request.prepare_audio(&pcm, &windows[0])?;
    let body = build_native_stt_request_body(&request, &audio)?;
    std::fs::write(out.join("packed.wav"), audio.wav_bytes())?;
    let started = Instant::now();
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        .build()?
        .post("https://openrouter.ai/api/v1/audio/transcriptions")
        .bearer_auth(resolve_api_key()?)
        .json(&body)
        .send()?;
    let status = response.status().as_u16();
    let generation = response
        .headers()
        .get("x-generation-id")
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);
    let mut bytes = Vec::new();
    response.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 4 * 1024 * 1024, "response too large");
    let latency = started.elapsed().as_millis() as u64;
    std::fs::write(out.join("response.private.json"), &bytes)?;
    let payload: serde_json::Value = serde_json::from_slice(&bytes)?;
    let result = decode_native_stt_body(&bytes, audio.duration_ms(), generation.clone(), latency);
    let metadata = json!({"modelRequested":model,"httpStatus":status,"generationId":generation,"latencyMs":latency,
        "audioDurationMs":audio.duration_ms(),"inputPaddingMs":audio.padding_ms(),"audioSha256":hex::encode(Sha256::digest(audio.wav_bytes())),
        "language":payload.get("language"),"duration":payload.get("duration"),"usage":payload.get("usage"),
        "segments":payload.get("segments").and_then(|s|s.as_array()).map(|s|s.iter().map(|s|json!({"start":s.get("start"),"end":s.get("end")})).collect::<Vec<_>>()),
        "decoderOutcome":match result.outcome {rcdvlog_core::review_speech::NativeSttOutcome::Usable(_)=>"usable".to_owned(),rcdvlog_core::review_speech::NativeSttOutcome::Unusable(error)=>error.code().to_owned()}});
    std::fs::write(
        out.join("receipt.json"),
        serde_json::to_vec_pretty(&metadata)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&metadata)?);
    Ok(())
}
