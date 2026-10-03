// Chat — one conversation, several model backends.
//
// Anthropic goes through claude.rs (native Messages API, server-side web search,
// files as document/image/text blocks). OpenRouter, DeepSeek and any custom
// endpoint go through the OpenAI-compatible client in openai.rs.
//
// Everything happens here rather than in the island: the API key never leaves
// the keychain, and file bytes never cross the IPC boundary.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{claude, openai, providers, settings::Settings};

const SYSTEM_PROMPT: &str = "You are Mochi, a personal AI assistant living at the top of the user's screen. \
You have web search access and can help with absolutely anything — research, coding, finding places, recommendations, tasks, questions. \
Respond in the user's language. Be thorough and complete — use as much detail as the task requires. \
No markdown formatting (no **, no ##, no bullet dashes). Use plain text with line breaks.";

/// Added only when an ERP is connected. Without it a general model will happily
/// invent order counts, customer names and stock levels, and even claim it has
/// no access to the user's ERP while an authenticated Service Layer session sits
/// in the keychain. A wrong number here costs the user a real business decision,
/// so the rule is explicit: never state a fact about their data from memory.
const ERP_PROMPT: &str = "\n\nAn ERP (SAP Business One) is connected for this user. Their orders, \
invoices, deliveries, business partners, stock and accounting figures live in that ERP and are \
never in your training data. Never state, estimate or infer a fact about their business data \
— order counts, invoice totals, customer or vendor names, balances, stock levels. If asked, say \
that the figures come from their ERP and that the ERP chat reads them live, and name the report \
it would use. Explaining how SAP B1 works in general is fine; inventing their numbers is not.";

/// The system prompt for one request. `erp_connected` is decided in Rust from the
/// stored credentials, never by the front end, so it cannot be spoofed.
fn system_prompt(erp_connected: bool) -> String {
    if erp_connected {
        format!("{SYSTEM_PROMPT}{ERP_PROMPT}")
    } else {
        SYSTEM_PROMPT.to_string()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
}

/// One turn, kept in a provider-neutral shape and rebuilt into the dialect of
/// whichever backend the next request uses. Switching provider clears the
/// history, so an Anthropic-only raw block never leaks into an OpenAI payload.
#[derive(Clone)]
struct Turn {
    role: Role,
    text: String,
    /// Anthropic assistant blocks (tool_use / web_search results) echoed back.
    blocks: Option<Vec<Value>>,
    /// File / window context, attached to the first user turn only.
    context: Option<ChatContext>,
}

#[derive(Default)]
pub struct Chat {
    turns: Mutex<Vec<Turn>>,
    /// `provider\u{1f}model` the stored history was built for. A model change
    /// clears the history too: an Anthropic raw block is not valid in an
    /// OpenAI payload, and silently dropping turns reads as amnesia.
    provider: Mutex<Option<String>>,
}

impl Chat {
    pub fn reset(&self) {
        self.turns.lock().unwrap().clear();
        *self.provider.lock().unwrap() = None;
    }

    fn is_empty(&self) -> bool {
        self.turns.lock().unwrap().is_empty()
    }

    fn switch_backend_if_needed(&self, id: &str, model: &str) {
        let signature = format!("{id}\u{1f}{model}");
        let mut current = self.provider.lock().unwrap();
        if current.as_deref() != Some(signature.as_str()) {
            self.turns.lock().unwrap().clear();
            *current = Some(signature);
        }
    }

    fn push(&self, turn: Turn) {
        self.turns.lock().unwrap().push(turn);
    }

    fn pop(&self) {
        self.turns.lock().unwrap().pop();
    }

    fn snapshot(&self) -> Vec<Turn> {
        self.turns.lock().unwrap().clone()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatContext {
    File { name: String, path: String },
    Window { app_name: String, title: String, url: Option<String> },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReply {
    pub text: String,
}

/// One chat turn. Returns the assistant's text, or a message the island shows
/// in the note view.
pub async fn send(
    chat: &Chat,
    settings: &Settings,
    provider_id: &str,
    model: &str,
    query: String,
    context: Option<ChatContext>,
    erp_connected: bool,
) -> Result<ChatReply, String> {
    let provider = providers::resolve(settings, provider_id);

    if provider.needs_key() && provider.key.is_empty() {
        return Err(format!("{} API key missing. Open Settings → Models.", provider.name));
    }

    let model = if model.trim().is_empty() {
        provider.default_model.clone()
    } else {
        model.trim().to_string()
    };
    if model.is_empty() {
        return Err("Pick a model in Settings first.".into());
    }
    chat.switch_backend_if_needed(&provider.id, &model);

    let turn = Turn {
        role: Role::User,
        text: query,
        blocks: None,
        context: if chat.is_empty() { context } else { None },
    };
    chat.push(turn);

    let prompt = system_prompt(erp_connected);
    let key = Some(provider.key.clone()).filter(|k| !k.is_empty());
    let result = match provider.style {
        providers::ApiStyle::Anthropic => {
            send_anthropic(chat, &provider, &key, &model, &prompt).await
        }
        providers::ApiStyle::OpenAICompatible => {
            send_openai(chat, &provider, &key, &model, &prompt).await
        }
    };

    if result.is_err() {
        chat.pop(); // keep the history consistent with what the model saw
    }
    result.map(|text| ChatReply { text })
}

// ── Anthropic ─────────────────────────────────────────────────────────────────

async fn send_anthropic(
    chat: &Chat,
    provider: &providers::Resolved,
    key: &Option<String>,
    model: &str,
    system: &str,
) -> Result<String, String> {
    let key = key.as_deref().ok_or("Anthropic API key missing. Open Settings → Models.")?;
    if provider.base().is_empty() {
        return Err("Set the endpoint URL for Anthropic first.".into());
    }
    let reply = claude::chat(provider.base(), model, key, system, &anthropic_messages(&chat.snapshot()))
        .await?;

    // Store the whole content — tool_use / tool_result blocks included — so the
    // next turn has the right context.
    chat.push(Turn {
        role: Role::Assistant,
        text: reply.text.clone(),
        blocks: Some(reply.blocks),
        context: None,
    });
    Ok(reply.text)
}

fn anthropic_messages(turns: &[Turn]) -> Vec<Value> {
    turns
        .iter()
        .map(|turn| match turn.role {
            Role::User => {
                let mut content: Vec<Value> = Vec::new();
                if let Some(ctx) = &turn.context {
                    match ctx {
                        ChatContext::File { name, path } => {
                            if let Some(block) = claude::file_block(path) {
                                content.push(block);
                            }
                            content.push(json!({ "type": "text", "text": format!("File: {name}") }));
                        }
                        ChatContext::Window { app_name, title, url } => {
                            let mut text = format!("Context — App: {app_name}, Window: {title}");
                            if let Some(url) = url {
                                text.push_str(&format!(", URL: {url}"));
                            }
                            content.push(json!({ "type": "text", "text": text }));
                        }
                    }
                }
                content.push(json!({ "type": "text", "text": turn.text }));
                json!({ "role": "user", "content": content })
            }
            Role::Assistant => {
                let content = turn
                    .blocks
                    .clone()
                    .unwrap_or_else(|| vec![json!({ "type": "text", "text": turn.text })]);
                json!({ "role": "assistant", "content": content })
            }
        })
        .collect()
}

// ── OpenAI-compatible ─────────────────────────────────────────────────────────

async fn send_openai(
    chat: &Chat,
    provider: &providers::Resolved,
    key: &Option<String>,
    model: &str,
    system: &str,
) -> Result<String, String> {
    let mut messages = vec![json!({ "role": "system", "content": system })];
    for turn in chat.snapshot() {
        match turn.role {
            Role::User => messages.push(json!({
                "role": "user",
                "content": openai_user_content(&turn),
            })),
            Role::Assistant => messages.push(json!({
                "role": "assistant",
                "content": turn.text,
            })),
        }
    }

    let base_url = provider.base();
    if base_url.is_empty() {
        return Err("Set the endpoint URL in Settings first.".into());
    }
    let text = openai::chat(base_url, key.as_deref(), model, &messages).await?;
    chat.push(Turn {
        role: Role::Assistant,
        text: text.clone(),
        blocks: None,
        context: None,
    });
    Ok(text)
}

/// A user turn as OpenAI content: a plain string, or a text + image parts array
/// when the context is an image.
fn openai_user_content(turn: &Turn) -> Value {
    let Some(ctx) = &turn.context else {
        return json!(turn.text);
    };
    match ctx {
        ChatContext::Window { app_name, title, url } => {
            let mut text = format!("Context — App: {app_name}, Window: {title}");
            if let Some(u) = url {
                text.push_str(&format!(", URL: {u}"));
            }
            json!(format!("{text}\n\n{}", turn.text))
        }
        ChatContext::File { name, path } => {
            if let Some((media, data)) = claude::image_data_uri(path) {
                json!([
                    { "type": "text", "text": format!("File: {name}\n\n{}", turn.text) },
                    { "type": "image_url", "image_url": { "url": format!("data:{media};base64,{data}") } },
                ])
            } else if let Some(text) = claude::inline_file_text(path) {
                json!(format!("File contents:\n{text}\n\nFile: {name}\n\n{}", turn.text))
            } else {
                json!(format!("File: {name}\n\n{}", turn.text))
            }
        }
    }
}

/// Model list for the Settings picker. Works for Anthropic and every
/// OpenAI-compatible backend; returns the raw endpoint shape (`{id, …}` objects).
pub async fn list_models(settings: &Settings, provider_id: &str) -> Result<Vec<Value>, String> {
    let provider = providers::resolve(settings, provider_id);
    if provider.needs_key() && provider.key.is_empty() {
        return Err("No API key — add it in Settings.".into());
    }
    if provider.base().is_empty() {
        return Err("Set the endpoint URL in Settings first.".into());
    }
    match provider.style {
        providers::ApiStyle::Anthropic => claude::models(provider.base(), &provider.key).await,
        providers::ApiStyle::OpenAICompatible => {
            openai::models(provider.base(), Some(provider.key.as_str())).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_erp_rule_only_appears_with_an_erp() {
        assert!(!system_prompt(false).contains("ERP"));
        assert!(system_prompt(true).contains("SAP Business One"));
        assert!(system_prompt(true).contains("Never state"));
    }

    #[test]
    fn the_base_prompt_still_allows_general_help() {
        // "How do I create an order in SAP B1" must still be answerable.
        assert!(system_prompt(true).contains("Explaining how SAP B1 works in general is fine"));
    }
}
