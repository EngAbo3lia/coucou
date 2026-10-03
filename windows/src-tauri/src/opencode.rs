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

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

// ── Sessions ──────────────────────────────────────────────────────────────────
// The plugin reports live session ids over the relay; the CLI lists the rest.
// Together they let the island show recent sessions and resume one exactly.

/// Ids seen in plugin events, so the list can mark the running ones.
static LIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn live() -> &'static Mutex<HashSet<String>> {
    LIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Records an opencode session id reported by the plugin (no-op when empty).
pub fn note_session(id: &str) {
    if id.is_empty() {
        return;
    }
    if let Ok(mut set) = live().lock() {
        set.insert(id.to_string());
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpencodeSession {
    pub id: String,
    pub title: String,
    pub directory: String,
    pub updated: i64,
    pub created: i64,
    pub live: bool,
}

#[derive(Deserialize)]
struct RawSession {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    directory: String,
    #[serde(default)]
    updated: i64,
    #[serde(default)]
    created: i64,
}

fn opencode_exe() -> Result<PathBuf, String> {
    platform::find_on_path("opencode").ok_or_else(|| "opencode is not on PATH".to_string())
}

/// Most recent sessions, newest first, from `opencode session list --format json`.
pub fn sessions(limit: u16) -> Result<Vec<OpencodeSession>, String> {
    let n = limit.clamp(1, 50).to_string();
    let mut cmd = Command::new(opencode_exe()?);
    cmd.args(["session", "list", "-n", &n, "--format", "json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    platform::no_console(&mut cmd);
    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("opencode session list failed".into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let raw: Vec<RawSession> =
        serde_json::from_str(&text).map_err(|e| format!("bad session list json: {e}"))?;
    let live = live().lock().map(|s| s.clone()).unwrap_or_default();
    let mut list: Vec<OpencodeSession> = raw
        .into_iter()
        .map(|r| OpencodeSession {
            live: live.contains(&r.id),
            id: r.id,
            title: r.title,
            directory: r.directory,
            updated: r.updated,
            created: r.created,
        })
        .collect();
    list.sort_by(|a, b| b.updated.cmp(&a.updated));
    Ok(list)
}

/// Resumes a session: brings an existing opencode window forward, or opens the
/// session in a new terminal. Returns true when a window was focused.
pub fn continue_session(session_id: &str, directory: &str) -> Result<bool, String> {
    if session_id.is_empty() {
        return Err("missing session id".into());
    }
    if focus_opencode_window(directory) {
        return Ok(true);
    }
    spawn_terminal(session_id, directory)?;
    Ok(false)
}

/// New terminal for a session: a Windows Terminal tab when available, a plain
/// console otherwise. Unlike the hook helpers this one must be *visible*.
/// Answers in an existing session without opening a terminal:
/// `opencode run -s <id> --format json <message>`. When `model` is non-empty it is
/// passed as `-m provider/model`, so Coucou can drive a session with the model the
/// user bound to it. The NDJSON stream is read tolerantly — the reply is the last
/// text field seen, whatever the shape — so a schema change in opencode does not
/// break the chat.
pub fn run(session_id: &str, directory: &str, message: &str, model: &str) -> Result<String, String> {
    if session_id.is_empty() {
        return Err("missing session id".into());
    }
    if message.trim().is_empty() {
        return Err("empty message".into());
    }
    let mut cmd = Command::new(opencode_exe()?);
    cmd.arg("run").arg("-s").arg(session_id).arg("--format").arg("json");
    if !model.trim().is_empty() {
        cmd.arg("-m").arg(model.trim());
    }
    cmd.arg(message)
        .stdin(Stdio::null())
        .stderr(Stdio::piped());
    if !directory.is_empty() && std::path::Path::new(directory).is_dir() {
        cmd.current_dir(directory);
    }
    platform::no_console(&mut cmd);
    let out = cmd.output().map_err(|e| e.to_string())?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut reply = String::new();
    for line in stdout.lines() {
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            if let Some(text) = find_text(&value) {
                if !text.trim().is_empty() {
                    reply = text;
                }
            }
        }
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("opencode run failed");
        return Err(last.trim().to_string());
    }
    if reply.trim().is_empty() {
        reply = stdout.trim().to_string();
    }
    if reply.trim().is_empty() {
        return Err("opencode returned nothing".into());
    }
    Ok(reply)
}

/// First non-empty string under a `text`/`content`/`message` key, anywhere in
/// the value tree. That covers a lone `text` field and a nested `part.text`.
fn find_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => items.iter().find_map(find_text),
        Value::Object(map) => {
            for key in ["text", "content", "message"] {
                if let Some(Value::String(s)) = map.get(key) {
                    if !s.trim().is_empty() {
                        return Some(s.clone());
                    }
                }
            }
            map.values().find_map(find_text)
        }
        _ => None,
    }
}

fn spawn_terminal(session_id: &str, directory: &str) -> Result<(), String> {
    let exe = opencode_exe()?.to_string_lossy().to_string();
    if let Some(wt) = platform::find_on_path("wt") {
        let opened = Command::new(wt)
            .args(["-w", "0", "new-tab", "--title", "opencode", "-d", directory])
            .arg(&exe)
            .args(["-s", session_id])
            .current_dir(directory)
            .spawn()
            .is_ok();
        if opened {
            return Ok(());
        }
    }
    Command::new("cmd")
        .args(["/C", "start", "", &exe, "-s", session_id])
        .current_dir(directory)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Best effort: match a visible window whose title carries "opencode" and, when
/// known, the project folder. Windows Terminal shares one window across tabs, so
/// a miss just means we spawn a new tab instead.
#[cfg(windows)]
fn focus_opencode_window(directory: &str) -> bool {
    use ::windows::core::BOOL;
    use ::windows::Win32::Foundation::{HWND, LPARAM};
    use ::windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible, SetForegroundWindow,
        ShowWindow, SW_RESTORE,
    };

    struct Ctx {
        needle: String,
        found: bool,
    }

    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = unsafe { &mut *(lparam.0 as *mut Ctx) };
        if ctx.found || !unsafe { IsWindowVisible(hwnd).as_bool() } {
            return BOOL(1);
        }
        let len = unsafe { GetWindowTextLengthW(hwnd) };
        if len <= 0 {
            return BOOL(1);
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
        if n <= 0 {
            return BOOL(1);
        }
        let title = String::from_utf16_lossy(&buf[..n as usize]).to_lowercase();
        if title.contains("opencode") && (ctx.needle.is_empty() || title.contains(&ctx.needle)) {
            unsafe {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
            ctx.found = true;
            return BOOL(0);
        }
        BOOL(1)
    }

    let leaf = std::path::Path::new(directory)
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut ctx = Ctx { needle: leaf, found: false };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    ctx.found
}

#[cfg(not(windows))]
fn focus_opencode_window(_directory: &str) -> bool {
    false
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
