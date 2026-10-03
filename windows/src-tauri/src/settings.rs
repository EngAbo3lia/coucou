// Preferences, stored as plain JSON in settings.json under platform::config_dir().
// No secret ever lands here — API keys live in the OS keychain (see secrets.rs).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::providers::unique_slug;

/// One AI backend the user has configured. Every config owns its own endpoint and
/// its own keychain entry, so "which provider" and "which key" stop being one
/// global answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ProviderConfig {
    /// Stable slug. Also the seed for `key_ref`, so it must never change.
    pub id: String,
    pub name: String,
    /// "anthropic" or "openai" — which request dialect this endpoint speaks.
    pub style: String,
    /// Base URL without the trailing endpoint. Empty means "not configured yet".
    pub base_url: String,
    /// Keychain entry holding the API key. Never the key itself.
    pub key_ref: String,
    pub default_model: String,
    /// False for local endpoints (Ollama, LM Studio) that need no key.
    pub key_required: bool,
    /// Built-ins cannot be deleted, only renamed and re-pointed.
    pub built_in: bool,
    /// Model ids the user starred. Kept here rather than in the model cache, so
    /// clearing the cache cannot lose a pin.
    pub pinned_models: Vec<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            style: crate::providers::API_OPENAI.to_string(),
            base_url: String::new(),
            key_ref: String::new(),
            default_model: String::new(),
            key_required: true,
            built_in: false,
            pinned_models: Vec::new(),
        }
    }
}

/// Which backend one agent talks to. Only used for requests Coucou starts
/// itself (chat, ERP, opencode) — an external Claude Code or VS Code session owns
/// its own model choice, so its binding is display-only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AgentBinding {
    /// Pill id, e.g. `integration_sapb1`.
    pub agent_id: String,
    /// `ProviderConfig.id`.
    pub provider: String,
    /// Model id, empty means "use the provider default".
    pub model: String,
}

impl Default for AgentBinding {
    fn default() -> Self {
        Self {
            agent_id: String::new(),
            provider: String::new(),
            model: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
// Container-level default, so a settings.json written by an older build loads
// with its new fields empty instead of failing to parse and silently resetting
// every other preference the user set.
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub sound_enabled: bool,
    pub sound_volume: f64,
    pub auto_close_interval: f64,
    pub absence_interval: f64,
    /// The four visible pill slots. Pill *visibility* lives in `features`.
    pub active_integrations: Vec<String>,
    /// "primary" = the main display, "cursor" = whichever display the mouse is on.
    pub screen: String,
    pub autostart: bool,
    pub hooks_installed: bool,

    // ── Chat backends ────────────────────────────────────────────────────────
    /// Saved backend configs. Empty on a pre-`providers` settings.json, which is
    /// then migrated from the legacy fields below.
    pub providers: Vec<ProviderConfig>,
    /// Per-agent backend bindings, keyed by pill id.
    pub agents: Vec<AgentBinding>,
    /// The backend the island chat uses. A legacy field name that also doubles as
    /// the default chat binding.
    pub provider: String,
    /// Model id for `provider`. Empty means "use the provider default".
    pub model: String,
    /// Legacy: base URL for the `custom` backend. Now `providers[].base_url`.
    pub custom_base_url: String,

    // ── Presentation ─────────────────────────────────────────────────────────
    /// Master switch for the pill row. Off means a compact, pill-free island.
    pub pills_visible: bool,
    /// Prefixed feature flags: `pill.<id>`, `integration.<id>`, `feature.<name>`.
    /// Every absent key is on, so this map only ever stores deliberate choices.
    pub features: BTreeMap<String, bool>,
}

fn default_provider() -> String {
    crate::providers::DEFAULT_PROVIDER_ID.to_string()
}

fn default_model() -> String {
    crate::providers::PRESETS
        .iter()
        .find(|p| p.id == default_provider())
        .map(|p| p.default_model.to_string())
        .unwrap_or_default()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sound_enabled: true,
            sound_volume: 0.12,
            auto_close_interval: 15.0,
            absence_interval: 180.0,
            active_integrations: vec![
                "integration_resend".into(),
                "integration_n8n".into(),
                "integration_vercel".into(),
                "integration_github".into(),
            ],
            screen: "primary".into(),
            autostart: false,
            hooks_installed: false,
            providers: crate::providers::PRESETS.iter().map(|p| p.config()).collect(),
            agents: Vec::new(),
            provider: default_provider(),
            model: default_model(),
            custom_base_url: String::new(),
            pills_visible: true,
            features: BTreeMap::new(),
        }
    }
}

impl Settings {
    /// A feature flag. Absent means on: the map records deliberate choices only,
    /// so a build that adds a new flag does not silently switch it off for
    /// everyone who already has a settings.json.
    pub fn feature(&self, key: &str) -> bool {
        self.features.get(key).copied().unwrap_or(true)
    }

    /// Whether an integration is switched on. Falls back to the visible-slot
    /// list for keys the map never held, which is what pre-flag builds used.
    pub fn integration_on(&self, id: &str) -> bool {
        if let Some(on) = self.features.get(&format!("integration.{id}")) {
            return *on;
        }
        id == "integration_claude"
            || id == "integration_sapb1"
            || self.active_integrations.contains(&id.to_string())
    }

    /// Whether a pill may appear in the notch.
    pub fn pill_on(&self, id: &str) -> bool {
        self.pills_visible && self.feature(&format!("pill.{id}"))
    }

    pub fn provider_config(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }

    pub fn provider_config_mut(&mut self, id: &str) -> Option<&mut ProviderConfig> {
        self.providers.iter_mut().find(|p| p.id == id)
    }

    /// A new user-defined backend, keyed on a slug that collides with nothing
    /// already saved. `None` when the name is empty, which the dialog blocks.
    pub fn add_provider(&mut self, name: &str, style: &str, base_url: &str) -> Option<String> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let taken: Vec<String> = self.providers.iter().map(|p| p.id.clone()).collect();
        let id = unique_slug(name, &taken);
        self.providers.push(ProviderConfig {
            id: id.clone(),
            name: name.to_string(),
            style: style.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            key_ref: format!("provider-{id}"),
            default_model: String::new(),
            key_required: true,
            built_in: false,
            pinned_models: Vec::new(),
        });
        Some(id)
    }

    /// Removes a user backend and every binding pointing at it. Built-ins are
    /// kept: deleting one would strand its stored key.
    pub fn remove_provider(&mut self, id: &str) -> bool {
        if self.providers.iter().any(|p| p.id == id && p.built_in) {
            return false;
        }
        let before = self.providers.len();
        self.providers.retain(|p| p.id != id);
        if self.providers.len() == before {
            return false;
        }
        if self.provider == id {
            self.provider = default_provider();
            self.model = String::new();
        }
        for b in &mut self.agents {
            if b.provider == id {
                b.provider = self.provider.clone();
                b.model = String::new();
            }
        }
        true
    }

    /// Duplicates a saved backend under a new name, with no key of its own: the
    /// user pastes a second one or leaves it empty for a keyless endpoint.
    pub fn duplicate_provider(&mut self, id: &str, name: &str) -> Option<String> {
        let source = self.provider_config(id)?.clone();
        let name = if name.trim().is_empty() { format!("{} copy", source.name) } else { name.to_string() };
        let new_id = self.add_provider(&name, &source.style, &source.base_url)?;
        if let Some(cfg) = self.provider_config_mut(&new_id) {
            cfg.default_model = source.default_model;
            cfg.key_required = source.key_required;
        }
        Some(new_id)
    }

    pub fn binding(&self, agent_id: &str) -> Option<&AgentBinding> {
        self.agents.iter().find(|a| a.agent_id == agent_id)
    }

    /// The binding for one agent, falling back to the global chat backend when
    /// the agent has none of its own.
    pub fn binding_or_default(&self, agent_id: &str) -> (String, String) {
        if let Some(b) = self.binding(agent_id) {
            if !b.provider.is_empty() && self.provider_config(&b.provider).is_some() {
                return (b.provider.clone(), b.model.clone());
            }
        }
        (self.provider.clone(), self.model.clone())
    }

    /// Turns a settings.json written before multi-provider support into one with
    /// a real `providers` list, keeping the keychain entries it already used.
    fn migrate(&mut self) {
        if self.providers.is_empty() {
            self.providers = crate::providers::PRESETS.iter().map(|p| p.config()).collect();
        }
        // The old `custom` backend kept its endpoint in a global field.
        if !self.custom_base_url.is_empty() {
            if let Some(custom) = self.providers.iter_mut().find(|p| p.id == "custom") {
                if custom.base_url.is_empty() {
                    custom.base_url = self.custom_base_url.clone();
                }
            }
        }
        // An unknown provider id used to fall back to Anthropic. Do it once, here,
        // so the settings window shows what chat will actually use.
        if self.provider_config(&self.provider).is_none() {
            self.provider = default_provider();
            self.model = default_model();
        }
        if let Some(cfg) = self.provider_config(&self.provider) {
            if self.model.is_empty() {
                self.model = cfg.default_model.clone();
            }
        }
        let known: Vec<String> = self.providers.iter().map(|p| p.id.clone()).collect();
        let (fallback_provider, fallback_model) = (self.provider.clone(), self.model.clone());
        for b in &mut self.agents {
            if !known.iter().any(|k| *k == b.provider) {
                // A binding that pointed at a deleted backend behaves as if it
                // never had one: the agent goes back to the global backend,
                // model included.
                b.provider = fallback_provider.clone();
                b.model = fallback_model.clone();
            }
        }
        let slots = self.active_integrations.clone();
        let flags = self.features.clone();
        self.active_integrations.retain(|id| {
            id.starts_with("integration_")
                && slots.contains(id)
                && flags
                    .get(&format!("integration.{id}"))
                    .copied()
                    .unwrap_or(true)
        });
        self.active_integrations.dedup();
        self.active_integrations.truncate(4);
    }
}

pub use crate::platform::{config_dir, local_dir};

pub fn hook_exe_path() -> PathBuf {
    local_dir().join("bin").join(crate::platform::HOOK_EXE)
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn load() -> Settings {
    let mut settings = match std::fs::read(settings_path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Settings::default(),
    };
    settings.migrate();
    settings
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let dir = config_dir();
    crate::platform::ensure_private_dir(&dir)?;
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(settings_path(), json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Parses settings.json the way `load` does: container default, then migrate.
    fn parse(bytes: &[u8]) -> Settings {
        let mut s: Settings = serde_json::from_slice(bytes).unwrap_or_default();
        s.migrate();
        s
    }

    #[test]
    fn old_settings_keep_their_preferences() {
        // Every new field absent. Before the container default this failed to
        // parse and threw away soundEnabled, screen and the pill slots too.
        let s = parse(
            br#"{"soundEnabled":false,"soundVolume":0.5,"autoCloseInterval":40,
                 "absenceInterval":200,"activeIntegrations":["integration_github"],
                 "screen":"cursor","autostart":true,"hooksInstalled":true}"#,
        );
        assert!(!s.sound_enabled);
        assert_eq!(s.sound_volume, 0.5);
        assert_eq!(s.auto_close_interval, 40.0);
        assert_eq!(s.screen, "cursor");
        assert!(s.autostart);
        assert!(s.hooks_installed);
    }

    #[test]
    fn old_settings_gain_every_preset() {
        let s = parse(br#"{"screen":"primary"}"#);
        assert!(s.providers.iter().any(|p| p.id == "anthropic"));
        assert!(s.providers.iter().any(|p| p.id == "opencode"));
        assert!(s.providers.iter().any(|p| p.id == "openai"));
        assert!(s.pills_visible, "a build that adds a flag must not switch it off");
    }

    #[test]
    fn legacy_custom_url_moves_into_the_custom_provider() {
        let s = parse(br#"{"provider":"custom","customBaseUrl":"http://localhost:11434/v1/"}"#);
        let custom = s.provider_config("custom").unwrap();
        assert_eq!(custom.base_url, "http://localhost:11434/v1/");
        assert_eq!(s.provider, "custom");
    }

    #[test]
    fn legacy_keychain_entries_are_not_orphaned() {
        let s = parse(br#"{"provider":"deepseek","model":"deepseek-reasoner"}"#);
        let d = s.provider_config("deepseek").unwrap();
        // The stored key must still be found where the old build put it.
        assert_eq!(d.key_ref, "deepseek-api-key");
        assert_eq!(s.model, "deepseek-reasoner");
    }

    #[test]
    fn unknown_provider_falls_back_once() {
        let s = parse(br#"{"provider":"does-not-exist","model":"nope"}"#);
        assert_eq!(s.provider, "anthropic");
        assert_ne!(s.model, "nope");
    }

    #[test]
    fn empty_model_takes_the_provider_default() {
        let s = parse(br#"{"provider":"anthropic","model":""}"#);
        assert!(!s.model.is_empty());
    }

    #[test]
    fn absent_flags_are_on_and_stored_choices_win() {
        let s = parse(br#"{"features":{"feature.motion":false,"pill.integration_vscode":false}}"#);
        assert!(s.feature("feature.sounds"), "absent means on");
        assert!(!s.feature("feature.motion"));
        assert!(s.pill_on("integration_github"));
        assert!(!s.pill_on("integration_vscode"));
    }

    #[test]
    fn pill_switch_hides_without_disabling_the_integration() {
        let s = parse(br#"{"features":{"pill.integration_github":false}}"#);
        assert!(!s.pill_on("integration_github"));
        assert!(s.integration_on("integration_github"), "still polls");
    }

    #[test]
    fn integration_switch_falls_back_to_the_legacy_slot_list() {
        let legacy = parse(br#"{"activeIntegrations":["integration_github","integration_n8n"]}"#);
        assert!(legacy.integration_on("integration_github"));
        assert!(legacy.integration_on("integration_n8n"));
        assert!(!legacy.integration_on("integration_vercel"));
        assert!(legacy.integration_on("integration_claude"), "always on");

        let explicit = parse(br#"{"features":{"integration.integration_github":false}}"#);
        assert!(!explicit.integration_on("integration_github"));
    }

    #[test]
    fn pills_off_hides_every_pill() {
        let s = parse(br#"{"pillsVisible":false}"#);
        assert!(!s.pill_on("integration_claude"));
        assert!(!s.pill_on("integration_sapb1"));
    }

    #[test]
    fn binding_wins_over_the_global_backend_and_falls_back_when_broken() {
        let s = parse(
            &serde_json::to_vec(&json!({
                "provider": "deepseek",
                "model": "deepseek-chat",
                "agents": [
                    { "agentId": "integration_sapb1", "provider": "opencode", "model": "big-pickle" },
                    { "agentId": "integration_opencode", "provider": "gone", "model": "" }
                ]
            }))
            .unwrap(),
        );
        assert_eq!(
            s.binding_or_default("integration_sapb1"),
            ("opencode".into(), "big-pickle".into())
        );
        assert_eq!(
            s.binding_or_default("integration_opencode"),
            ("deepseek".into(), "deepseek-chat".into()),
            "a binding pointing at a deleted provider falls back"
        );
    }

    #[test]
    fn every_provider_has_a_unique_id_and_keychain_entry() {
        let mut ids = std::collections::BTreeSet::new();
        let mut keys = std::collections::BTreeSet::new();
        for p in &Settings::default().providers {
            assert!(ids.insert(p.id.clone()), "duplicate id {}", p.id);
            assert!(keys.insert(p.key_ref.clone()), "duplicate key {}", p.key_ref);
            assert!(
                crate::secrets::is_allowed(&p.key_ref),
                "{} has a keychain entry the store would refuse",
                p.id
            );
            assert!(
                !p.base_url.contains("chat/completions"),
                "{} must store the base URL, not the full endpoint",
                p.id
            );
        }
    }

    #[test]
    fn round_trips_through_json() {
        let mut s = Settings::default();
        s.model = "big-pickle".into();
        s.pills_visible = false;
        s.features.insert("feature.motion".into(), false);
        let bytes = serde_json::to_vec(&s).unwrap();
        assert_eq!(parse(&bytes).model, "big-pickle");
        assert!(!parse(&bytes).pills_visible);
        assert!(!parse(&bytes).feature("feature.motion"));
    }

    #[test]
    fn a_new_backend_gets_its_own_keychain_entry() {
        let mut s = Settings::default();
        let id = s.add_provider("My Gateway", "openai", "http://localhost:8080/v1/").unwrap();
        assert_eq!(id, "my-gateway");
        let cfg = s.provider_config(&id).unwrap();
        assert_eq!(cfg.key_ref, "provider-my-gateway");
        assert!(crate::secrets::is_allowed(&cfg.key_ref));
        assert_eq!(cfg.base_url, "http://localhost:8080/v1", "trailing slash trimmed");
        assert!(!cfg.built_in);
    }

    #[test]
    fn a_blank_backend_name_is_refused() {
        let mut s = Settings::default();
        assert!(s.add_provider("   ", "openai", "http://x/v1").is_none());
    }

    #[test]
    fn two_backends_from_the_same_name_do_not_collide() {
        let mut s = Settings::default();
        let a = s.add_provider("Gateway", "openai", "http://a/v1").unwrap();
        let b = s.add_provider("Gateway", "openai", "http://b/v1").unwrap();
        assert_eq!((a.as_str(), b.as_str()), ("gateway", "gateway-2"));
        assert_ne!(s.provider_config("gateway").unwrap().key_ref, s.provider_config("gateway-2").unwrap().key_ref);
    }

    #[test]
    fn deleting_a_backend_repoints_everything_that_used_it() {
        let mut s = Settings::default();
        let id = s.add_provider("Temp", "openai", "http://x/v1").unwrap();
        s.provider = id.clone();
        s.agents.push(AgentBinding {
            agent_id: "integration_opencode".into(),
            provider: id.clone(),
            model: "big-pickle".into(),
        });
        assert!(s.remove_provider(&id));
        assert!(s.provider_config(&id).is_none());
        assert_eq!(s.provider, "anthropic");
        assert_eq!(s.binding("integration_opencode").unwrap().provider, "anthropic");
        assert_eq!(s.binding("integration_opencode").unwrap().model, "");
    }

    #[test]
    fn a_builtin_backend_cannot_be_deleted() {
        let mut s = Settings::default();
        assert!(!s.remove_provider("anthropic"));
        assert!(s.provider_config("anthropic").is_some());
    }

    #[test]
    fn deleting_something_absent_reports_false() {
        let mut s = Settings::default();
        assert!(!s.remove_provider("never-existed"));
    }

    #[test]
    fn duplicating_copies_the_endpoint_but_not_the_key() {
        let mut s = Settings::default();
        let id = s.duplicate_provider("opencode", "OpenCode staging").unwrap();
        let cfg = s.provider_config(&id).unwrap();
        assert_eq!(cfg.base_url, "https://opencode.ai/zen/v1");
        assert_eq!(cfg.key_ref, format!("provider-{id}"), "the copy needs its own key");
        assert!(!cfg.built_in);
    }
}