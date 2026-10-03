// Model catalogue — what each saved backend can be asked to answer with.
//
// Two sources, and the difference matters:
//   - `opencode models <provider> --verbose` reports cost, context and
//     capabilities per model. That is the only place free-vs-paid is real data
//     rather than a guess from the model name.
//   - An OpenAI-compatible `/models` endpoint reports ids only. Those entries
//     carry `free: None`, and the UI prints "—" instead of inventing a price.
//
// Results are cached in models-cache.json under the config dir so the models page
// opens instantly and still works offline. Pins live in settings, not here, so
// losing the cache cannot lose a pin.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::providers;
use crate::settings::Settings;

/// One row of the models table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ModelEntry {
    /// Model id as the endpoint expects it, e.g. `big-pickle`.
    pub id: String,
    /// Human name, when the source reports one.
    pub label: String,
    pub provider: String,
    /// Context window in tokens, when reported.
    pub context: Option<u64>,
    /// USD per million input tokens, when reported. 0 means genuinely free.
    pub cost_in: Option<f64>,
    /// USD per million output tokens, when reported.
    pub cost_out: Option<f64>,
    /// None = the source did not say. Only Some(false) means "known to cost".
    pub free: Option<bool>,
}

impl Default for ModelEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            provider: String::new(),
            context: None,
            cost_in: None,
            cost_out: None,
            free: None,
        }
    }
}

impl ModelEntry {
    /// Display name: the reported label when there is one, else the id.
    pub fn display(&self) -> &str {
        if self.label.is_empty() {
            &self.id
        } else {
            &self.label
        }
    }

    /// `provider/model`, the form `opencode run -m` and most gateways want.
    pub fn qualified(&self) -> String {
        if self.provider.is_empty() {
            self.id.clone()
        } else {
            format!("{}/{}", self.provider, self.id)
        }
    }

    fn from_json(provider: &str, value: &serde_json::Value) -> Option<ModelEntry> {
        let id = value
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())?
            .to_string();
        let limit = value.get("limit");
        let cost = value.get("cost");
        let cost_in = cost.and_then(|c| c.get("input")).and_then(|v| v.as_f64());
        let cost_out = cost.and_then(|c| c.get("output")).and_then(|v| v.as_f64());
        let both_free = match (cost_in, cost_out) {
            (Some(0.0), Some(0.0)) => Some(true),
            (Some(0.0), None) | (None, Some(0.0)) => Some(true),
            (Some(_), _) => Some(false),
            _ => None,
        };
        Some(ModelEntry {
            label: value
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&id)
                .to_string(),
            id,
            provider: value
                .get("providerID")
                .and_then(|v| v.as_str())
                .unwrap_or(provider)
                .to_string(),
            context: limit.and_then(|l| l.get("context")).and_then(|v| v.as_u64()),
            cost_in,
            cost_out,
            free: both_free,
        })
    }
}

// ── Cache ─────────────────────────────────────────────────────────────────────

/// provider id → models, plus when each was fetched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Catalog {
    providers: BTreeMap<String, Vec<ModelEntry>>,
    fetched_at: BTreeMap<String, String>,
}

fn cache_path() -> PathBuf {
    crate::settings::config_dir().join("models-cache.json")
}

fn load_cache() -> Catalog {
    std::fs::read(cache_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_cache(cache: &Catalog) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(cache).map_err(|e| e.to_string())?;
    crate::platform::ensure_private_dir(&crate::settings::config_dir()).map_err(|e| e.to_string())?;
    std::fs::write(cache_path(), json).map_err(|e| e.to_string())
}

/// Cached models for one backend, if any were ever fetched.
pub fn cached(provider: &str) -> Vec<ModelEntry> {
    load_cache().providers.get(provider).cloned().unwrap_or_default()
}

/// When a backend's list was last refreshed, for the "updated 2m ago" label.
pub fn fetched_at(provider: &str) -> Option<String> {
    load_cache().fetched_at.get(provider).cloned()
}

/// Every cached backend, for a first paint with no network.
pub fn all_cached() -> Vec<(String, Vec<ModelEntry>)> {
    load_cache().providers.into_iter().collect()
}

pub fn forget(provider: &str) {
    let mut cache = load_cache();
    cache.providers.remove(provider);
    cache.fetched_at.remove(provider);
    let _ = save_cache(&cache);
}

// ── Fetching ──────────────────────────────────────────────────────────────────

/// Live model list for one backend, then cached.
///
/// Cost data only arrives from opencode's own catalogue. Everywhere else the ids
/// are real and the prices are honestly absent, rather than guessed from a name.
pub async fn refresh(settings: &Settings, provider_id: &str) -> Result<Vec<ModelEntry>, String> {
    let resolved = providers::resolve(settings, provider_id);
    let entries: Vec<ModelEntry> = if resolved.id == "opencode" {
        from_opencode_cli(&resolved.id).await?
    } else {
        let raw = crate::chat::list_models(settings, &resolved.id).await?;
        if raw.is_empty() {
            return Err("The endpoint reported no models.".into());
        }
        raw.into_iter()
            .filter_map(|v| ModelEntry::from_json(&resolved.id, &v))
            .collect()
    };

    let mut entries: Vec<ModelEntry> = entries
        .into_iter()
        .map(|mut e| {
            e.provider = resolved.id.clone();
            e
        })
        .filter(|e| !e.id.is_empty())
        .collect();
    // Stable order so a re-fetch does not reshuffle the table under the cursor.
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    entries.dedup_by(|a, b| a.id == b.id);

    let mut cache = load_cache();
    cache.providers.insert(resolved.id.clone(), entries.clone());
    cache
        .fetched_at
        .insert(resolved.id.clone(), now_iso());
    let _ = save_cache(&cache);
    Ok(entries)
}

/// Parses `opencode models <provider> --verbose`.
///
/// The output is a bare model id, then a pretty-printed JSON object per model —
/// not JSONL — so this walks the stream and collects brace-balanced blocks
/// instead of assuming one object per line.
fn parse_verbose_models(provider: &str, text: &str) -> Vec<ModelEntry> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut block = String::new();
    for line in text.lines() {
        if depth == 0 {
            if line.trim_start().starts_with('{') {
                depth = 1;
                block = line.to_string();
            }
            continue;
        }
        block.push('\n');
        block.push_str(line);
        depth += line.matches('{').count();
        depth = depth.saturating_sub(line.matches('}').count());
        if depth > 0 {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&block) {
            if let Some(entry) = ModelEntry::from_json(provider, &value) {
                out.push(entry);
            }
        }
        block.clear();
    }
    out
}

async fn from_opencode_cli(provider: &str) -> Result<Vec<ModelEntry>, String> {
    let exe = crate::platform::find_on_path("opencode").ok_or_else(|| {
        "OpenCode Zen needs the opencode CLI on PATH to read its model catalogue.".to_string()
    })?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["models", provider, "--verbose"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    crate::platform::no_console(&mut cmd);
    let out = tokio::task::spawn_blocking(move || cmd.output())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("Could not read the opencode model catalogue.".into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let entries = parse_verbose_models(provider, &text);
    if entries.is_empty() {
        return Err("opencode reported no models for this provider.".into());
    }
    Ok(entries)
}

// ── Pins ──────────────────────────────────────────────────────────────────────

/// Pinned model ids, kept in settings so they survive a cache wipe.
pub fn pinned(settings: &Settings) -> Vec<String> {
    settings
        .providers
        .iter()
        .flat_map(|p| p.pinned_models.clone())
        .collect()
}

/// Pins `id` for `provider`, and unpins it when `on` is false.
pub fn set_pin(settings: &mut Settings, provider: &str, id: &str, on: bool) -> Result<(), String> {
    let cfg = settings
        .provider_config_mut(provider)
        .ok_or_else(|| format!("Unknown backend {provider}"))?;
    cfg.pinned_models.retain(|m| m != id);
    if on {
        cfg.pinned_models.push(id.to_string());
    }
    Ok(())
}

/// The catalog page: cached entries for every backend, sorted so pinned rows and
/// free rows are easy to find.
pub fn catalog(settings: &Settings) -> Vec<ModelEntry> {
    let mut all: Vec<ModelEntry> = Vec::new();
    for (id, entries) in all_cached() {
        all.extend(entries.into_iter().map(|mut e| {
            e.provider = id.clone();
            e
        }));
    }
    all.sort_by(|a, b| {
        let pa = is_pinned(settings, &a.provider, &a.id);
        let pb = is_pinned(settings, &b.provider, &b.id);
        pb.cmp(&pa)
            .then_with(|| a.provider.cmp(&b.provider))
            .then_with(|| a.id.cmp(&b.id))
    });
    all
}

pub fn is_pinned(settings: &Settings, provider: &str, id: &str) -> bool {
    settings
        .provider_config(provider)
        .map(|p| p.pinned_models.iter().any(|m| m == id))
        .unwrap_or(false)
}

/// Set as the default model for a backend. Rejects an id that is not in the
/// catalogue, so a typo cannot leave chat pointing at nothing.
pub fn set_default_model(settings: &mut Settings, provider: &str, id: &str) -> Result<(), String> {
    if !cached(provider).iter().any(|m| m.id == id) {
        return Err(format!("{id} is not in the {provider} model list."));
    }
    match settings.provider_config_mut(provider) {
        Some(cfg) => {
            cfg.default_model = id.to_string();
            Ok(())
        }
        None => Err(format!("Unknown backend {provider}")),
    }
}

fn now_iso() -> String {
    // No date-time dependency in the build; a unix timestamp is enough to show
    // "updated 2m ago" and needs no formatting crate.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERBOSE: &str = r#"opencode/big-pickle
{
  "id": "big-pickle",
  "providerID": "opencode",
  "name": "Big Pickle",
  "api": {
    "id": "big-pickle",
    "url": "https://opencode.ai/zen/v1"
  },
  "status": "active",
  "cost": {
    "input": 0,
    "output": 0,
    "cache": {
      "read": 0,
      "write": 0
    }
  },
  "limit": {
    "context": 200000,
    "input": 160000,
    "output": 32000
  }
}
opencode/claude-opus-4
{
  "id": "claude-opus-4",
  "providerID": "opencode",
  "name": "Claude Opus 4",
  "cost": {
    "input": 15,
    "output": 75
  },
  "limit": {
    "context": 200000
  }
}
"#;

    #[test]
    fn verbose_blocks_are_parsed_in_full() {
        let models = parse_verbose_models("opencode", VERBOSE);
        assert_eq!(models.len(), 2, "both objects, not just the first");
        assert_eq!(models[0].id, "big-pickle");
        assert_eq!(models[0].label, "Big Pickle");
        assert_eq!(models[0].context, Some(200_000));
        assert_eq!(models[1].id, "claude-opus-4");
        assert_eq!(models[1].cost_in, Some(15.0));
        assert_eq!(models[1].cost_out, Some(75.0));
    }

    #[test]
    fn zero_cost_from_the_catalogue_means_free() {
        let models = parse_verbose_models("opencode", VERBOSE);
        assert_eq!(models[0].free, Some(true));
        assert_eq!(models[1].free, Some(false));
    }

    #[test]
    fn a_bare_id_list_reports_no_price_rather_than_guessing_one() {
        // An OpenAI-compatible /models response has ids and nothing else.
        let value: serde_json::Value =
            serde_json::from_str(r#"[{"id":"gpt-4o"},{"id":"gpt-4o-mini"}]"#).unwrap();
        let entries: Vec<ModelEntry> = value
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| ModelEntry::from_json("openai", v))
            .collect();
        assert_eq!(entries.len(), 2);
        for e in &entries {
            assert_eq!(e.free, None, "unknown is not the same as paid");
            assert_eq!(e.cost_in, None);
            assert_eq!(e.context, None);
        }
        assert_eq!(entries[0].display(), "gpt-4o", "falls back to the id");
    }

    #[test]
    fn a_paid_model_is_never_labelled_free() {
        let value: serde_json::Value =
            serde_json::from_str(r#"[{"id":"m","cost":{"input":0.4,"output":1.2}}]"#).unwrap();
        let e = ModelEntry::from_json("x", &value[0]).unwrap();
        assert_eq!(e.free, Some(false));
    }

    #[test]
    fn an_entry_with_no_id_is_skipped() {
        let value: serde_json::Value = serde_json::from_str(r#"[{"id":""},{"name":"x"}]"#).unwrap();
        let entries: Vec<ModelEntry> = value
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| ModelEntry::from_json("x", v))
            .collect();
        assert!(entries.is_empty());
    }

    #[test]
    fn a_qualified_id_carries_the_backend() {
        let e = ModelEntry { id: "big-pickle".into(), provider: "opencode".into(), ..Default::default() };
        assert_eq!(e.qualified(), "opencode/big-pickle");
        let bare = ModelEntry { id: "gpt-4o".into(), ..Default::default() };
        assert_eq!(bare.qualified(), "gpt-4o");
    }

    #[test]
    fn pins_add_remove_and_do_not_duplicate() {
        let mut s = Settings::default();
        set_pin(&mut s, "opencode", "big-pickle", true).unwrap();
        set_pin(&mut s, "opencode", "big-pickle", true).unwrap();
        assert_eq!(pinned(&s), vec!["big-pickle".to_string()]);
        assert!(is_pinned(&s, "opencode", "big-pickle"));
        set_pin(&mut s, "opencode", "big-pickle", false).unwrap();
        assert!(!is_pinned(&s, "opencode", "big-pickle"));
        assert!(pinned(&s).is_empty());
    }

    #[test]
    fn pinning_an_unknown_backend_is_refused() {
        let mut s = Settings::default();
        assert!(set_pin(&mut s, "nope", "m", true).is_err());
    }

    #[test]
    fn a_model_not_in_the_catalogue_cannot_become_the_default() {
        let mut s = Settings::default();
        let err = set_default_model(&mut s, "opencode", "not-a-real-model").unwrap_err();
        assert!(err.contains("not in the"), "{err}");
        assert!(set_default_model(&mut s, "opencode", "also-real-model").is_err());
    }

    #[test]
    fn truncated_json_does_not_panic() {
        assert!(parse_verbose_models("opencode", "").is_empty());
        assert!(parse_verbose_models("opencode", "{ \"id\": \"half\"").is_empty());
        assert!(parse_verbose_models("opencode", "not json at all").is_empty());
    }
}