// Chat provider registry — every model backend Coucou can talk to.
//
// Two API styles are supported:
//
//   - Anthropic: the native Messages API, with server-side web search.
//   - OpenAI-compatible: `POST /chat/completions`, which covers OpenRouter,
//     DeepSeek, Ollama, LM Studio, vLLM, Groq, Together and anything else that
//     speaks the same dialect.
//
// Settings only ever store the provider id, the model id and (for a custom
// endpoint) the base URL. Keys live in the OS keychain (see secrets.rs).

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiStyle {
    Anthropic,
    OpenAICompatible,
}

impl ApiStyle {
    fn as_str(self) -> &'static str {
        match self {
            ApiStyle::Anthropic => "anthropic",
            ApiStyle::OpenAICompatible => "openai",
        }
    }
}

/// Serialised to the settings window so the provider list has a single source of
/// truth in Rust.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub accent: &'static str,
    pub default_model: &'static str,
    pub key: &'static str,
    pub key_required: bool,
    pub style: &'static str,
}

pub fn all_info() -> Vec<ProviderInfo> {
    PROVIDERS
        .iter()
        .map(|p| ProviderInfo {
            id: p.id,
            name: p.name,
            accent: p.accent,
            default_model: p.default_model,
            key: p.key,
            key_required: p.key_required,
            style: p.style.as_str(),
        })
        .collect()
}

pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    /// Pill / selector accent, same palette style as the rest of the app.
    pub accent: &'static str,
    pub style: ApiStyle,
    /// Base URL without the trailing endpoint, e.g. `https://api.openai.com/v1`.
    /// Empty for `custom`, which reads its base URL from settings.
    pub base_url: &'static str,
    /// Keychain entry that holds the API key.
    pub key: &'static str,
    /// Model shown before the user fetches the live list.
    pub default_model: &'static str,
    /// A local endpoint (Ollama, LM Studio) needs no key.
    pub key_required: bool,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "anthropic",
        name: "Anthropic",
        accent: "#E07950",
        style: ApiStyle::Anthropic,
        base_url: "https://api.anthropic.com",
        key: "anthropic-api-key",
        default_model: crate::claude::DEFAULT_MODEL,
        key_required: true,
    },
    Provider {
        id: "openrouter",
        name: "OpenRouter",
        accent: "#6467F2",
        style: ApiStyle::OpenAICompatible,
        base_url: "https://openrouter.ai/api/v1",
        key: "openrouter-api-key",
        default_model: "deepseek/deepseek-chat",
        key_required: true,
    },
    Provider {
        id: "deepseek",
        name: "DeepSeek",
        accent: "#4D6BFE",
        style: ApiStyle::OpenAICompatible,
        base_url: "https://api.deepseek.com/v1",
        key: "deepseek-api-key",
        default_model: "deepseek-chat",
        key_required: true,
    },
    Provider {
        id: "custom",
        name: "Custom",
        accent: "#22C55E",
        style: ApiStyle::OpenAICompatible,
        base_url: "",
        key: "custom-api-key",
        default_model: "",
        key_required: false,
    },
];

/// The provider used when settings carry an unknown or missing id.
pub const DEFAULT_PROVIDER_ID: &str = "anthropic";

pub fn find(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// Resolves a settings provider id to a known provider, falling back to Anthropic.
pub fn resolve(id: &str) -> &'static Provider {
    find(id).unwrap_or_else(|| find(DEFAULT_PROVIDER_ID).expect("anthropic is in the registry"))
}

impl Provider {
    /// Full base URL, honouring the user's custom endpoint. Always trimmed of a
    /// trailing slash so `/chat/completions` can be appended safely.
    pub fn base_url_from(&self, custom_base_url: &str) -> String {
        let raw = if self.id == "custom" { custom_base_url } else { self.base_url };
        raw.trim_end_matches('/').to_string()
    }

    /// Whether a key must be present before sending.
    pub fn needs_key(&self) -> bool {
        self.key_required || self.id == "custom"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_falls_back_to_anthropic() {
        assert_eq!(resolve("").id, "anthropic");
        assert_eq!(resolve("nope").id, "anthropic");
        assert_eq!(resolve("openrouter").id, "openrouter");
    }

    #[test]
    fn base_url_uses_settings_for_custom_only() {
        let custom = find("custom").unwrap();
        assert_eq!(custom.base_url_from("http://localhost:11434/v1/"), "http://localhost:11434/v1");
        let or = find("openrouter").unwrap();
        assert_eq!(or.base_url_from("http://ignored"), "https://openrouter.ai/api/v1");
    }
}
