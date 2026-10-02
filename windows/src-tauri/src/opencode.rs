// opencode integration.
//
// opencode has no hooks file like Claude Code — it loads plugins from
// `~/.config/opencode/plugins/`. "Installing the hooks" therefore means copying
// the Coucou plugin there. The plugin tags every event with
// `coucou_agent: "opencode"`, so opencode sessions get their own pill in the
// island, exactly like Gemini or Codex.
//
// The plugin is embedded in the binary at build time, so there is nothing to
// ship alongside it.

use std::path::PathBuf;

use serde::Serialize;

use crate::platform;

/// The plugin shipped with the app. Kept in the repo so it can also be copied
/// by hand; embedded here so the app can install it without a resource file.
pub const PLUGIN: &str = include_str!("../../../integrations/opencode/coucou.js");

/// Unique text that identifies a plugin file as ours, for safe removal.
const MARKER: &str = "Coucou ⇄ opencode bridge";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeStatus {
    /// A Coucou plugin is present (ours), whatever its version.
    pub installed: bool,
    /// The present plugin is byte-for-byte the one this build ships.
    pub up_to_date: bool,
    pub plugin_path: String,
    pub config_dir: String,
}

fn config_dir() -> PathBuf {
    platform::home_dir().join(".config").join("opencode")
}

fn plugin_path() -> PathBuf {
    config_dir().join("plugins").join("coucou.js")
}

/// True when a file's text is one of ours (present or past versions).
fn is_ours(text: &str) -> bool {
    text.contains(MARKER)
}

pub fn status() -> OpenCodeStatus {
    let path = plugin_path();
    let text = std::fs::read_to_string(&path).ok();
    OpenCodeStatus {
        installed: text.as_deref().map(is_ours).unwrap_or(false),
        up_to_date: text.as_deref() == Some(PLUGIN),
        plugin_path: path.to_string_lossy().to_string(),
        config_dir: config_dir().to_string_lossy().to_string(),
    }
}

/// Copies the plugin into the opencode config. A different existing coucou.js —
/// the user's own, or an older build — is backed up first, never silently lost.
/// Returns the backup path, or an empty string when there was nothing to back up.
pub fn install() -> Result<String, String> {
    let path = plugin_path();
    let dir = path.parent().ok_or("bad plugin path")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;

    let mut backup = String::new();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if existing == PLUGIN {
            return Ok(backup); // already installed, nothing to do
        }
        if !existing.is_empty() {
            let dest = path.with_file_name(format!("coucou.js.bak-{}", stamp()));
            std::fs::write(&dest, existing).map_err(|e| e.to_string())?;
            backup = dest.to_string_lossy().to_string();
        }
    }

    std::fs::write(&path, PLUGIN).map_err(|e| e.to_string())?;
    Ok(backup)
}

/// Removes the plugin, but only when it is ours. A file the user replaced is
/// left alone rather than deleted.
pub fn uninstall() -> Result<(), String> {
    let path = plugin_path();
    match std::fs::read_to_string(&path) {
        Ok(text) if is_ours(&text) => std::fs::remove_file(&path).map_err(|e| e.to_string()),
        Ok(_) => Err("coucou.js is not Coucou's — remove it by hand if you want it gone.".into()),
        Err(_) => Ok(()), // nothing installed
    }
}

fn stamp() -> String {
    let t = platform::local_time();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

#[cfg(test)]
mod tests {
    use super::{is_ours, PLUGIN};

    #[test]
    fn ours_is_recognised_and_foreign_is_not() {
        assert!(is_ours(PLUGIN));
        assert!(!is_ours("// some other plugin\nexport const X = 1\n"));
    }
}
