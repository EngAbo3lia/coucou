// API keys live in the Windows Credential Manager or, on Linux, the Secret
// Service (GNOME Keyring, KWallet) — never on disk and never in the front end.
// The island and the settings window may only ask whether a key is present.

use keyring::Entry;

const SERVICE: &str = "fr.louisraille.coucou";

/// Every fixed key Coucou may store.
pub const KNOWN_KEYS: &[&str] = &[
    "anthropic-api-key",
    "openrouter-api-key",
    "deepseek-api-key",
    "custom-api-key",
    "n8n-url",
    "n8n-api-key",
    "vercel-token",
    "github-token",
    "stripe-api-key",
    "resend-api-key",
    "notion-api-key",
    "calcom-api-key",
    "sapb1-url",
    "sapb1-company",
    "sapb1-user",
    "sapb1-password",
];

/// Namespace for a user-defined backend: `provider-<slug>`. Any number of these
/// may exist, so the allowlist cannot be a fixed list.
const PROVIDER_PREFIX: &str = "provider-";

/// Whether the store will accept this key name. Fixed keys plus a well-formed
/// `provider-<slug>`, and nothing else — a name is never built from raw user
/// text without going through the slug rules below.
pub fn is_allowed(key: &str) -> bool {
    if KNOWN_KEYS.contains(&key) {
        return true;
    }
    match key.strip_prefix(PROVIDER_PREFIX) {
        Some(slug) => is_valid_slug(slug),
        None => false,
    }
}

/// A slug is lowercase alphanumerics and single dashes, 1..64 characters, and
/// never `.` or `/` — so a key name can never walk out of its namespace or reach
/// a file path.
fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn entry(key: &str) -> Option<Entry> {
    if !is_allowed(key) {
        return None;
    }
    Entry::new(SERVICE, key).ok()
}

pub fn get(key: &str) -> Option<String> {
    entry(key)?.get_password().ok().filter(|v| !v.is_empty())
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    if value.is_empty() {
        let _ = entry.delete_credential();
        return Ok(());
    }
    entry.set_password(value).map_err(|e| e.to_string())
}

pub fn clear(key: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn present(key: &str) -> bool {
    get(key).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fixed_key_is_allowed() {
        for k in KNOWN_KEYS {
            assert!(is_allowed(k), "{k} should be allowed");
        }
    }

    #[test]
    fn a_user_backend_key_is_allowed() {
        assert!(is_allowed("provider-my-gateway"));
        assert!(is_allowed("provider-opencode"));
        assert!(is_allowed("provider-ollama-2"));
    }

    #[test]
    fn an_odd_slug_is_refused() {
        for k in [
            "provider-",
            "provider--",
            "provider-Uppercase",
            "provider-has_underscore",
            "provider-with.dot",
            "provider-with/slash",
            "provider-..",
            "provider-a..b",
        ] {
            assert!(!is_allowed(k), "{k} should be refused");
        }
    }

    #[test]
    fn an_over_long_slug_is_refused() {
        assert!(!is_allowed(&format!("provider-{}", "a".repeat(80))));
        assert!(is_allowed(&format!("provider-{}", "a".repeat(60))));
    }

    #[test]
    fn an_unknown_fixed_key_is_refused() {
        assert!(!is_allowed("totally-made-up"));
        assert!(!is_allowed(""));
        assert!(!is_allowed("sapb1-Password"), "wrong case, wrong key");
    }

    #[test]
    fn a_name_without_the_prefix_cannot_reach_the_dynamic_branch() {
        // "providerx-opencode" must not be treated as a provider key.
        assert!(!is_allowed("providerx-opencode"));
        assert!(!is_allowed("xprovider-opencode"));
    }

    #[test]
    fn writes_are_refused_for_an_unknown_key() {
        assert!(set("totally-made-up", "value").is_err());
        assert!(clear("totally-made-up").is_err());
    }

    #[test]
    fn a_slug_from_the_provider_module_is_always_accepted() {
        for name in ["My Gateway", "../etc/passwd", "  ", "OLLAMA"] {
            let slug = crate::providers::slugify(name);
            assert!(
                is_allowed(&format!("{PROVIDER_PREFIX}{slug}")),
                "{name:?} slugified to {slug:?}, which the store would refuse"
            );
        }
    }
}