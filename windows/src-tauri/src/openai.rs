// OpenAI-compatible client — one implementation for OpenRouter, DeepSeek and any
// custom endpoint (Ollama, LM Studio, vLLM, Groq, Together…).
//
// The API key never leaves Rust: the island only calls the commands in lib.rs.

use serde_json::{json, Value};

const MAX_TOKENS: u32 = 4096;
const TIMEOUT_SECS: u64 = 90;

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .build()
        .map_err(|e| e.to_string())
}

/// Adds the bearer header only when a key exists, so keyless local servers work.
fn with_auth(req: reqwest::RequestBuilder, key: Option<&str>) -> reqwest::RequestBuilder {
    match key.filter(|k| !k.is_empty()) {
        Some(k) => req.bearer_auth(k),
        None => req,
    }
}

/// Surfaces the API's own error message, which is what makes a bad key obvious.
fn api_error(status: reqwest::StatusCode, text: &str) -> String {
    let detail = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| text.chars().take(200).collect());
    format!("{status}: {detail}")
}

/// One non-streaming chat completion. Returns the assistant's text.
pub async fn chat(
    base_url: &str,
    key: Option<&str>,
    model: &str,
    messages: &[Value],
) -> Result<String, String> {
    let body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "messages": messages,
    });

    let response = with_auth(
        client()?.post(format!("{base_url}/chat/completions")),
        key,
    )
    .json(&body)
    .send()
    .await
    .map_err(|e| format!("Network error: {e}"))?;

    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(api_error(status, &text));
    }

    let json: Value = serde_json::from_str(&text).map_err(|e| format!("Bad API response: {e}"))?;
    let content = json
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    if content.is_empty() {
        return Err("No response text.".into());
    }
    Ok(content)
}

/// Live model list for the picker. Works with every OpenAI-compatible `/models`
/// endpoint; `name` is used as the label when the provider sends one
/// (OpenRouter does), otherwise the id is shown.
pub async fn models(base_url: &str, key: Option<&str>) -> Result<Vec<Value>, String> {
    let response = with_auth(client()?.get(format!("{base_url}/models")), key)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(api_error(status, &text));
    }

    let json: Value = serde_json::from_str(&text).map_err(|e| format!("Bad API response: {e}"))?;
    let data = json
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut out = Vec::with_capacity(data.len());
    for item in data {
        let Some(id) = item.get("id").and_then(Value::as_str) else { continue };
        // OpenAI returns embeddings, TTS, image and audio models too; the chat
        // picker only wants conversation models.
        let lower = id.to_lowercase();
        if lower.contains("embed")
            || lower.contains("whisper")
            || lower.contains("tts")
            || lower.contains("dall-e")
            || lower.contains("audio")
            || lower.contains("image")
            || lower.contains("moderat")
            || lower.contains("sora")
            || lower.contains("realtime")
        {
            continue;
        }
        let label = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string();
        out.push(json!({ "id": id, "label": label }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::api_error;

    #[test]
    fn api_error_reads_the_provider_message() {
        let body = r#"{"error":{"message":"Invalid API key","type":"auth"}}"#;
        let msg = api_error(reqwest::StatusCode::UNAUTHORIZED, body);
        assert!(msg.contains("Invalid API key"), "{msg}");
    }
}
