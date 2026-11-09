//! Read-only OpenRouter catalog discovery for the review transcription picker.
use super::*;
use std::io::Read;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSttModelChoice {
    pub id: String,
    pub name: String,
}

type CatalogCache = Option<(Instant, Vec<NativeSttModelChoice>)>;
static MODELS: OnceLock<Mutex<CatalogCache>> = OnceLock::new();

/// Caches public metadata briefly; never sends audio or reads provider credentials.
pub fn discover_native_stt_models() -> Result<Vec<NativeSttModelChoice>> {
    let mut cache = MODELS
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| anyhow!("transcription catalog cache unavailable"))?;
    if let Some((fetched, models)) = cache.as_ref()
        && fetched.elapsed() < Duration::from_secs(300)
    {
        return Ok(models.clone());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .get("https://openrouter.ai/api/v1/models?output_modalities=transcription")
        .send()
        .map_err(|_| anyhow!("OpenRouter transcription catalog unavailable"))?;
    ensure!(
        response.status().is_success(),
        "OpenRouter transcription catalog returned HTTP {}",
        response.status().as_u16()
    );
    let mut body = Vec::new();
    response.take(2 * 1024 * 1024 + 1).read_to_end(&mut body)?;
    ensure!(
        body.len() <= 2 * 1024 * 1024,
        "transcription catalog too large"
    );
    let models = parse_models(&serde_json::from_slice(&body)?)?;
    *cache = Some((Instant::now(), models.clone()));
    Ok(models)
}

fn parse_models(value: &Value) -> Result<Vec<NativeSttModelChoice>> {
    let rows = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("invalid transcription catalog"))?;
    let mut result = Vec::new();
    for row in rows {
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        // Official STT docs explicitly exclude these models from verbose_json.
        if [
            "openai/gpt-4o-transcribe",
            "openai/gpt-4o-mini-transcribe",
            "microsoft/mai-transcribe-1.5",
        ]
        .contains(&id)
        {
            continue;
        }
        let contains = |value: Option<&Value>, expected: &str| {
            value
                .and_then(Value::as_array)
                .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(expected)))
        };
        if !contains(row.pointer("/architecture/input_modalities"), "audio")
            || !contains(
                row.pointer("/architecture/output_modalities"),
                "transcription",
            )
            || !contains(row.get("endpoints"), "/api/v1/audio/transcriptions")
            || NativeSttRequest::for_discovered_model(id.into(), None).is_err()
        {
            continue;
        }
        let name = row
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        result.push(NativeSttModelChoice {
            id: id.into(),
            name,
        });
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result.dedup_by(|a, b| a.id == b.id);
    ensure!(
        !result.is_empty(),
        "OpenRouter returned no eligible transcription models"
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transcription_discovery_requires_native_audio_endpoint_and_filters_unsupported_formats() {
        let row = |id: &str, outputs: &[&str], endpoints: &[&str]| {
            json!({"id":id,"name":id,
            "architecture":{"input_modalities":["audio"],"output_modalities":outputs},"endpoints":endpoints})
        };
        let rows = json!({"data":[
            row("google/gemini-3.5-transcribe", &["transcription"], &["/api/v1/audio/transcriptions"]),
            row("openai/gpt-4o", &["text"], &["/api/v1/chat/completions"]),
            row("openai/gpt-4o-transcribe", &["transcription"], &["/api/v1/audio/transcriptions"]),
            row("microsoft/mai-transcribe-1.5", &["transcription"], &["/api/v1/audio/transcriptions"]),
            row("invalid id", &["transcription"], &["/api/v1/audio/transcriptions"])]});
        assert_eq!(
            parse_models(&rows)
                .unwrap()
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            vec!["google/gemini-3.5-transcribe"]
        );
    }
}
