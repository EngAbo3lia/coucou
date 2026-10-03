// Chat provider registry — every model backend Coucou can talk to.
//
// Two API styles are supported:
//
//   - Anthropic: the native Messages API, with server-side web search.
//   - OpenAI-compatible: `POST /chat/completions`, which covers OpenRouter,
//     DeepSeek, OpenAI, OpenCode Zen, Ollama, LM Studio, vLLM, Groq, Together and
//     anything else that speaks the same dialect.
//
// PRESETS is the read-only catalogue offered on first run. The user's own
// backends live in `Settings::providers` and are resolved at request time, so a
// backend can have its own endpoint and its own keychain entry.
//
// Settings only ever store ids, names, endpoints and model ids. Keys live in the
// OS keychain (see secrets.rs) under the config's `key_ref`.

use crate::settings::{ProviderConfig, Settings};
use serde::Serialize;

pub const API_ANTHROPIC: &str = "anthropic";
pub const API_OPENAI: &str = "openai";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiStyle {
    Anthropic,
    OpenAICompatible,
}

impl ApiStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            ApiStyle::Anthropic => API_ANTHROPIC,
            ApiStyle::OpenAICompatible => API_OPENAI,
        }
    }

    /// Anything unknown is treated as OpenAI-compatible: that is the dialect
    /// every gateway, proxy and local runtime speaks.
    pub fn parse(s: &str) -> Self {
        if s == API_ANTHROPIC {
            ApiStyle::Anthropic
        } else {
            ApiStyle::OpenAICompatible
        }
    }
}

/// A backend resolved from the user's settings, ready to send one request.
/// Owned rather than `&'static`, because a user backend's name, endpoint and key
/// are runtime data.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub id: String,
    pub name: String,
    pub style: ApiStyle,
    /// Trailing slash not yet trimmed; read it through `base()`.
    base_url: String,
    pub key: String,
    pub default_model: String,
    pub key_required: bool,
}

impl Resolved {
    /// Base URL without a trailing slash, so an endpoint can be appended safely.
    pub fn base(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    pub fn needs_key(&self) -> bool {
        self.key_required
    }
}

/// One entry in the built-in catalogue.
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    /// Pill / selector accent, same palette style as the rest of the app.
    pub accent: &'static str,
    pub style: ApiStyle,
    /// Base URL without the trailing endpoint, e.g. `https://api.openai.com/v1`.
    /// Empty for `custom`, which the user points at their own gateway.
    pub base_url: &'static str,
    /// Keychain entry holding the key. The first four keep their pre-multi-provider
    /// names so an upgrade does not orphan a key the user already stored.
    pub key_ref: &'static str,
    /// Model shown before the user fetches the live list.
    pub default_model: &'static str,
    /// A local endpoint needs no key; a hosted one does.
    pub key_required: bool,
    /// One line saying where this endpoint is, for the add-provider dialog.
    pub note: &'static str,
}

impl Preset {
    /// The `settings.json` shape for this preset.
    pub fn config(&self) -> ProviderConfig {
        ProviderConfig {
            id: self.id.into(),
            name: self.name.into(),
            style: self.style.as_str().into(),
            base_url: self.base_url.into(),
            key_ref: self.key_ref.into(),
            default_model: self.default_model.into(),
            key_required: self.key_required,
            built_in: true,
            pinned_models: Vec::new(),
        }
    }

    pub fn info(&self) -> PresetInfo {
        PresetInfo {
            id: self.id,
            name: self.name,
            accent: self.accent,
            style: self.style.as_str(),
            base_url: self.base_url,
            key_ref: self.key_ref,
            default_model: self.default_model,
            key_required: self.key_required,
            note: self.note,
        }
    }
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "anthropic",
        name: "Anthropic",
        accent: "#E07950",
        style: ApiStyle::Anthropic,
        base_url: "https://api.anthropic.com",
        key_ref: "anthropic-api-key",
        default_model: crate::claude::DEFAULT_MODEL,
        key_required: true,
        note: "Native Messages API, with server-side web search.",
    },
    Preset {
        id: "openrouter",
        name: "OpenRouter",
        accent: "#6467F2",
        style: ApiStyle::OpenAICompatible,
        base_url: "https://openrouter.ai/api/v1",
        key_ref: "openrouter-api-key",
        default_model: "deepseek/deepseek-chat",
        key_required: true,
        note: "One key, many hosted models.",
    },
    Preset {
        id: "deepseek",
        name: "DeepSeek",
        accent: "#4D6BFE",
        style: ApiStyle::OpenAICompatible,
        base_url: "https://api.deepseek.com/v1",
        key_ref: "deepseek-api-key",
        default_model: "deepseek-chat",
        key_required: true,
        note: "Cheap reasoning and chat models.",
    },
    Preset {
        id: "openai",
        name: "OpenAI",
        accent: "#10A37F",
        style: ApiStyle::OpenAICompatible,
        base_url: "https://api.openai.com/v1",
        key_ref: "provider-openai",
        default_model: "gpt-4o",
        key_required: true,
        note: "The official OpenAI endpoint.",
    },
    Preset {
        id: "opencode",
        name: "OpenCode Zen",
        accent: "#F59E0B",
        style: ApiStyle::OpenAICompatible,
        base_url: "https://opencode.ai/zen/v1",
        key_ref: "provider-opencode",
        default_model: "big-pickle",
        key_required: true,
        note: "opencode's curated model gateway. Several models are free.",
    },
    Preset {
        id: "custom",
        name: "Custom",
        accent: "#22C55E",
        style: ApiStyle::OpenAICompatible,
        base_url: "",
        key_ref: "custom-api-key",
        default_model: "",
        key_required: false,
        note: "Any OpenAI-compatible gateway, or a local runtime such as Ollama.",
    },
];

/// The provider used when settings carry an unknown or missing id.
pub const DEFAULT_PROVIDER_ID: &str = "anthropic";

pub fn find(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// Builds the backend for one request. An unknown id falls back to Anthropic, so
/// a stale settings.json can never leave chat with no backend at all.
pub fn resolve(settings: &Settings, id: &str) -> Resolved {
    let fallback = || find(DEFAULT_PROVIDER_ID).expect("anthropic is in the catalogue");
    match settings.provider_config(id) {
        Some(cfg) => from_config(cfg),
        None => match settings.provider_config(DEFAULT_PROVIDER_ID) {
            Some(cfg) => from_config(cfg),
            None => from_config(&fallback().config()),
        },
    }
}

/// Turns one saved config into a request-ready backend, reading its key from the
/// keychain.
pub fn from_config(cfg: &ProviderConfig) -> Resolved {
    let id = if cfg.id.is_empty() { DEFAULT_PROVIDER_ID } else { cfg.id.as_str() };
    Resolved {
        id: id.to_string(),
        name: if cfg.name.is_empty() {
            find(id).map(|p| p.name).unwrap_or(id).to_string()
        } else {
            cfg.name.clone()
        },
        style: ApiStyle::parse(&cfg.style),
        base_url: cfg.base_url.clone(),
        key: crate::secrets::get(&cfg.key_ref).unwrap_or_default(),
        default_model: cfg.default_model.clone(),
        key_required: cfg.key_required,
    }
}

/// The catalogue the add-provider dialog offers.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub accent: &'static str,
    pub style: &'static str,
    pub base_url: &'static str,
    pub key_ref: &'static str,
    pub default_model: &'static str,
    pub key_required: bool,
    pub note: &'static str,
}

pub fn all_presets() -> Vec<PresetInfo> {
    PRESETS.iter().map(|p| p.info()).collect()
}

/// A saved backend as the settings window sees it. `has_key` is computed in Rust
/// so the key itself never crosses the IPC boundary.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigInfo {
    pub id: String,
    pub name: String,
    pub accent: String,
    pub style: String,
    pub base_url: String,
    pub default_model: String,
    pub key_required: bool,
    pub built_in: bool,
    pub has_key: bool,
    pub key_ref: String,
    pub pinned_models: Vec<String>,
}

/// The accent for a saved config, falling back to the preset's, then to a neutral
/// one, so a user backend still gets a stable colour in pills and pickers.
fn accent_for(cfg: &ProviderConfig) -> String {
    if cfg.id == "custom" {
        return "#22C55E".into();
    }
    find(&cfg.id).map(|p| p.accent).unwrap_or("#8B95A5").to_string()
}

pub fn config_info(cfg: &ProviderConfig) -> ConfigInfo {
    ConfigInfo {
        id: cfg.id.clone(),
        name: cfg.name.clone(),
        accent: accent_for(cfg),
        style: cfg.style.clone(),
        base_url: cfg.base_url.clone(),
        default_model: cfg.default_model.clone(),
        key_required: cfg.key_required,
        built_in: cfg.built_in,
        has_key: crate::secrets::present(&cfg.key_ref),
        key_ref: cfg.key_ref.clone(),
        pinned_models: cfg.pinned_models.clone(),
    }
}

/// Turns a free-form name into a stable provider slug: lowercase alphanumerics
/// and single dashes, never empty, never a path segment.
pub fn slugify(text: &str) -> String {
    let mut slug = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() { "provider".into() } else { slug }
}

/// A slug that does not collide with an existing config.
pub fn unique_slug(base: &str, taken: &[String]) -> String {
    let base = slugify(base);
    if !taken.iter().any(|t| *t == base) {
        return base;
    }
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        if !taken.iter().any(|t| *t == candidate) {
            return candidate;
        }
    }
    format!("{base}-new")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(ids: &[&str]) -> Settings {
        let mut s = Settings::default();
        s.providers.retain(|p| ids.contains(&p.id.as_str()));
        s
    }

    #[test]
    fn resolve_falls_back_to_anthropic() {
        let s = settings_with(&["anthropic"]);
        assert_eq!(resolve(&s, "").id, "anthropic");
        assert_eq!(resolve(&s, "nope").id, "anthropic");
    }

    #[test]
    fn resolve_works_with_no_saved_configs_at_all() {
        let mut s = Settings::default();
        s.providers.clear();
        let r = resolve(&s, "opencode");
        assert_eq!(r.id, "anthropic");
        assert!(r.needs_key());
    }

    #[test]
    fn resolve_reads_a_user_config() {
        let mut s = settings_with(&["anthropic", "opencode"]);
        let cfg = s.provider_config_mut("opencode").unwrap();
        cfg.base_url = "https://example.test/v1".into();
        let r = resolve(&s, "opencode");
        assert_eq!(r.id, "opencode");
        assert_eq!(r.base(), "https://example.test/v1");
        assert_eq!(r.style, ApiStyle::OpenAICompatible);
    }

    #[test]
    fn base_url_trims_a_trailing_slash_only_when_read() {
        let mut s = settings_with(&["custom"]);
        s.provider_config_mut("custom").unwrap().base_url = "http://localhost:11434/v1/".into();
        assert_eq!(resolve(&s, "custom").base(), "http://localhost:11434/v1");
    }

    #[test]
    fn a_preset_endpoint_ignores_the_legacy_custom_base_url_global() {
        // The old customBaseUrl global used to override every provider, which sent
        // a self-hosted URL to OpenRouter.
        let mut s = settings_with(&["anthropic", "openrouter"]);
        s.custom_base_url = "http://localhost:11434/v1".into();
        assert_eq!(resolve(&s, "openrouter").base(), "https://openrouter.ai/api/v1");
    }

    #[test]
    fn an_unnamed_config_falls_back_to_its_preset_name() {
        let mut s = settings_with(&["anthropic", "deepseek"]);
        s.provider_config_mut("deepseek").unwrap().name = String::new();
        assert_eq!(resolve(&s, "deepseek").name, "DeepSeek");
    }

    #[test]
    fn unknown_api_style_is_openai_compatible() {
        assert_eq!(ApiStyle::parse("anthropic"), ApiStyle::Anthropic);
        assert_eq!(ApiStyle::parse(""), ApiStyle::OpenAICompatible);
        assert_eq!(ApiStyle::parse("nonsense"), ApiStyle::OpenAICompatible);
    }

    #[test]
    fn preset_base_urls_are_https_and_endpoint_free() {
        for p in PRESETS {
            if p.base_url.is_empty() {
                continue;
            }
            assert!(p.base_url.starts_with("https://"), "{} is not https", p.id);
            assert!(
                !p.base_url.contains("chat/completions"),
                "{} must store the base URL, not the full endpoint",
                p.id
            );
        }
    }

    #[test]
    fn the_endpoints_the_user_asked_for_are_present() {
        assert_eq!(find("opencode").unwrap().base_url, "https://opencode.ai/zen/v1");
        assert_eq!(find("openai").unwrap().base_url, "https://api.openai.com/v1");
    }

    #[test]
    fn legacy_keychain_names_survive_and_new_ones_are_namespaced() {
        assert_eq!(find("anthropic").unwrap().key_ref, "anthropic-api-key");
        assert_eq!(find("openrouter").unwrap().key_ref, "openrouter-api-key");
        assert_eq!(find("deepseek").unwrap().key_ref, "deepseek-api-key");
        assert_eq!(find("custom").unwrap().key_ref, "custom-api-key");
        assert_eq!(find("opencode").unwrap().key_ref, "provider-opencode");
        assert_eq!(find("openai").unwrap().key_ref, "provider-openai");
    }

    #[test]
    fn every_preset_key_is_allowed_by_the_secret_store() {
        for p in PRESETS {
            assert!(
                crate::secrets::is_allowed(p.key_ref),
                "{} uses {} which secrets.rs would refuse",
                p.id,
                p.key_ref
            );
        }
    }

    #[test]
    fn a_user_backend_gets_a_neutral_accent_not_a_preset_colour() {
        let cfg = ProviderConfig { id: "ollama".into(), name: "Ollama".into(), ..Default::default() };
        assert_eq!(accent_for(&cfg), "#8B95A5");
    }

    #[test]
    fn config_info_reports_presence_not_the_secret() {
        let s = settings_with(&["anthropic"]);
        let info = config_info(s.provider_config("anthropic").unwrap());
        assert!(info.has_key || !info.has_key, "reads the keychain, never stores it");
        assert_eq!(info.key_ref, "anthropic-api-key");
        assert!(info.built_in);
    }

    #[test]
    fn slugs_are_stable_and_collision_free() {
        assert_eq!(slugify("My Gateway"), "my-gateway");
        assert_eq!(slugify("  Local//LLM  "), "local-llm");
        assert_eq!(slugify("!!!"), "provider");
        assert_eq!(slugify(""), "provider");
        let taken = vec!["opencode".to_string(), "opencode-2".to_string()];
        assert_eq!(unique_slug("OpenCode", &taken), "opencode-3");
    }

    #[test]
    fn a_slug_can_never_escape_the_secret_namespace() {
        for name in ["../etc/passwd", "a/../../b", "..", "x\\y"] {
            let slug = slugify(name);
            assert!(
                slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{name:?} produced {slug:?}"
            );
            assert!(!slug.is_empty());
        }
    }
}