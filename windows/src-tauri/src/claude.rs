// Anthropic Messages API client — the Claude backend of the chat.
//
// The conversation history and the provider routing live in chat.rs; this file
// only knows how to talk to Anthropic: multi-turn messages with server-side web
// search, plus the file-block helpers shared by the other backends.
//
// The API key never leaves Rust: the island only calls the commands in lib.rs.

use serde_json::{json, Value};

const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Server-side fallback: on a policy decline the API retries the same request on
/// a fallback model inside the same call, so the island never shows a dead end.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MAX_TOKENS: u32 = 4096;
/// Text and code files are inlined; anything larger is skipped, as on macOS.
const MAX_INLINE_TEXT: u64 = 200_000;

/// Default Claude model for a fresh install.
pub const DEFAULT_MODEL: &str = "claude-opus-5";

/// The assistant's answer: its text, and the raw content blocks so the next turn
/// can echo tool_use / web_search results back to the model.
pub struct Reply {
    pub text: String,
    pub blocks: Vec<Value>,
}

/// One Anthropic turn. `messages` is already in Anthropic's `{role, content}` shape.
pub async fn chat(
    base_url: &str,
    model: &str,
    key: &str,
    system: &str,
    messages: &[Value],
) -> Result<Reply, String> {
    let body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": system,
        "tools": [{ "type": "web_search_20260209", "name": "web_search", "max_uses": 5 }],
        "fallbacks": "default",
        "messages": messages,
    });

    let response = call(base_url, key, &body).await?;

    // A policy decline comes back as HTTP 200 with stop_reason "refusal".
    if response.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        let why = response
            .get("stop_details")
            .and_then(|d| d.get("explanation"))
            .and_then(Value::as_str)
            .unwrap_or("Claude declined this one.");
        return Err(why.to_string());
    }

    let Some(blocks) = response.get("content").and_then(Value::as_array).cloned() else {
        return Err("Unexpected API response.".into());
    };

    let text = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if text.is_empty() {
        return Err("No response text.".into());
    }
    Ok(Reply { text, blocks })
}

/// One deterministic turn: no web-search tool, no fallback banner. Used where
/// the model must answer a fixed prompt (the ERP planner), not browse.
/// `messages` is already in Anthropic's `{role, content}` shape, so the planner
/// can hand over the whole conversation and the model stays aware of it.
pub async fn chat_plain(
    base_url: &str,
    model: &str,
    key: &str,
    system: &str,
    messages: &[Value],
) -> Result<String, String> {
    let body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": system,
        "messages": messages,
    });
    let response = call(base_url, key, &body).await?;
    let Some(blocks) = response.get("content").and_then(Value::as_array).cloned() else {
        return Err("Unexpected API response.".into());
    };
    let text = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if text.is_empty() {
        return Err("No response text.".into());
    }
    Ok(text)
}

async fn call(base_url: &str, key: &str, body: &Value) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .post(format!("{base_url}/v1/messages"))
        .header("x-api-key", key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("anthropic-beta", FALLBACK_BETA)
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        // Surface the API's own message, which is what makes a bad key obvious.
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| text.chars().take(200).collect());
        return Err(format!("Claude API {status}: {detail}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("Bad API response: {e}"))
}

/// Live model list for the Anthropic picker.
pub async fn models(base_url: &str, key: &str) -> Result<Vec<Value>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(format!("{base_url}/v1/models?limit=100"))
        .header("x-api-key", key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("Anthropic {status}"));
    }
    let json: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let items = json.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
    Ok(items
        .iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(Value::as_str)?;
            let label = item.get("display_name").and_then(Value::as_str).unwrap_or(id);
            Some(json!({ "id": id, "label": label }))
        })
        .collect())
}

/// PDF → document block, image → image block, text/code → inline text.
/// Mirrors readFileAsBlock() in ClaudeService.swift.
pub fn file_block(path: &str) -> Option<Value> {
    if let Some((block_type, media)) = binary_media_type(path) {
        let bytes = std::fs::read(path).ok()?;
        return Some(json!({
            "type": block_type,
            "source": { "type": "base64", "media_type": media, "data": base64(&bytes) },
        }));
    }
    let text = inline_file_text(path)?;
    Some(json!({ "type": "text", "text": format!("File contents:\n{text}") }))
}

/// `(block_type, media_type)` for files sent as binary, or None for text.
pub fn binary_media_type(path: &str) -> Option<(&'static str, &'static str)> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "pdf" => Some(("document", "application/pdf")),
        "jpg" | "jpeg" => Some(("image", "image/jpeg")),
        "png" => Some(("image", "image/png")),
        "gif" => Some(("image", "image/gif")),
        "webp" => Some(("image", "image/webp")),
        _ => None,
    }
}

/// `(media_type, base64)` for image files only — used to build image_url parts.
pub fn image_data_uri(path: &str) -> Option<(&'static str, String)> {
    let (block_type, media) = binary_media_type(path)?;
    if block_type != "image" {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some((media, base64(&bytes)))
}

/// Inlines a text/code file up to the size cap, or None.
pub fn inline_file_text(path: &str) -> Option<String> {
    let len = std::fs::metadata(path).ok()?.len();
    if len > MAX_INLINE_TEXT {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Small standalone base64 encoder — not worth another dependency.
/// Also used for Stripe's basic auth.
pub(crate) fn base64_for(bytes: &[u8]) -> String {
    base64(bytes)
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
