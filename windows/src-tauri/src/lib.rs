// Coucou for Windows — app wiring and the commands the island calls.

mod chat;
mod claude;
mod files;
mod hooks;
mod integrations;
mod island;
mod log;
mod models;
mod openai;
mod opencode;
mod pipe;
mod platform;
mod providers;
mod sapb1;
mod secrets;
mod settings;
mod tray;

use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::{ManagerExt, MacosLauncher};

use chat::{Chat, ChatContext, ChatReply};
use files::DroppedFile;
use hooks::{HookPreview, HookStatus};
use island::{PollGate, ScreenInfo};
use pipe::Pending;
use settings::{AgentBinding, ProviderConfig, Settings};

pub struct Shared {
    pub settings: Mutex<Settings>,
    pub gate: Arc<PollGate>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootInfo {
    settings: Settings,
    screen: ScreenInfo,
    version: String,
    hook_path: String,
    /// True when Coucou's opencode plugin is installed, so the island can show
    /// the opencode pill instead of the unused Claude Code one.
    opencode_installed: bool,
    /// False where the OS has no global cursor (Wayland): the page then reports
    /// the cursor from its own mouse events.
    cursor_poll: bool,
}

#[tauri::command]
fn boot(app: AppHandle, shared: State<Shared>) -> BootInfo {
    let mut settings = shared.settings.lock().unwrap().clone();
    // The real state of ~/.claude/settings.json wins over whatever we stored.
    settings.hooks_installed = hooks::status().installed;
    let screen = island::screen_info(&app, &settings.screen);
    BootInfo {
        settings,
        screen,
        version: env!("CARGO_PKG_VERSION").to_string(),
        hook_path: settings::hook_exe_path().to_string_lossy().to_string(),
        opencode_installed: opencode::status().installed,
        cursor_poll: platform::CURSOR_POLL,
    }
}

#[tauri::command]
fn save_settings(app: AppHandle, shared: State<Shared>, settings: Settings) {
    let (screen_changed, autostart_changed) = {
        let mut current = shared.settings.lock().unwrap();
        let screen_changed = current.screen != settings.screen;
        let autostart_changed = current.autostart != settings.autostart;
        *current = settings.clone();
        (screen_changed, autostart_changed)
    };
    if let Err(err) = settings::save(&settings) {
        eprintln!("[coucou] could not save settings: {err}");
    }
    if autostart_changed {
        let manager = app.autolaunch();
        let result = if settings.autostart { manager.enable() } else { manager.disable() };
        if let Err(err) = result {
            eprintln!("[coucou] autostart: {err}");
        }
    }
    if screen_changed {
        let collapsed = shared.gate.collapsed.load(Ordering::Relaxed);
        let compact = shared.gate.compact.load(Ordering::Relaxed);
        let chat = shared.gate.chat.load(Ordering::Relaxed);
        island::apply_geometry(&app, &settings.screen, collapsed, compact, chat);
    }
    // Keep the other window in step (island ⇄ settings window).
    let _ = app.emit("settings-changed", settings);
}

/// Hidden island → shrink the window to the invisible wake strip and park the
/// cursor poll; anything else → full panel and 60 Hz polling.
#[tauri::command]
fn set_collapsed(app: AppHandle, shared: State<Shared>, collapsed: bool) {
    let pref = shared.settings.lock().unwrap().screen.clone();
    let compact = shared.gate.compact.load(Ordering::Relaxed);
    let chat = shared.gate.chat.load(Ordering::Relaxed);
    shared.gate.collapsed.store(collapsed, Ordering::Relaxed);
    island::apply_geometry(&app, &pref, collapsed, compact, chat);
    // The wake strip must always take the mouse, and a resize invalidates the flag.
    island::refresh_click_through(&app, &shared.gate);
    shared.gate.set_active(!collapsed);
}

/// The front end knows which pills are actually visible, so it tells Rust when
/// the island should shrink to a single centred pill.
#[tauri::command]
fn set_compact(app: AppHandle, shared: State<Shared>, compact: bool) {
    let pref = shared.settings.lock().unwrap().screen.clone();
    let collapsed = shared.gate.collapsed.load(Ordering::Relaxed);
    let chat = shared.gate.chat.load(Ordering::Relaxed);
    shared.gate.compact.store(compact, Ordering::Relaxed);
    if !collapsed {
        island::apply_geometry(&app, &pref, false, compact, chat);
        island::refresh_click_through(&app, &shared.gate);
    }
}

/// The chat is open, so the window grows taller to give the conversation room.
#[tauri::command]
fn set_chat_expanded(app: AppHandle, shared: State<Shared>, expanded: bool) {
    let pref = shared.settings.lock().unwrap().screen.clone();
    let collapsed = shared.gate.collapsed.load(Ordering::Relaxed);
    let compact = shared.gate.compact.load(Ordering::Relaxed);
    shared.gate.chat.store(expanded, Ordering::Relaxed);
    if !collapsed {
        island::apply_geometry(&app, &pref, false, compact, expanded);
        island::refresh_click_through(&app, &shared.gate);
    }
}

/// The front end pushes the island shape; Rust decides click-through from it.
#[tauri::command]
fn set_island_rect(app: AppHandle, shared: State<Shared>, x: f64, y: f64, width: f64, height: f64) {
    shared.gate.set_rect(island::IslandRect { x, y, w: width, h: height });
    // Without the cursor poll the input region is the click-through: it follows the island.
    if !platform::CURSOR_POLL {
        island::refresh_click_through(&app, &shared.gate);
    }
}

#[tauri::command]
fn focus_window(app: AppHandle, focused: bool) {
    let Some(win) = island::window(&app) else { return };
    platform::set_activating(&win, focused);
    if focused {
        let _ = win.set_focus();
    }
}

#[tauri::command]
fn reposition(app: AppHandle, shared: State<Shared>) {
    let pref = shared.settings.lock().unwrap().screen.clone();
    let collapsed = shared.gate.collapsed.load(Ordering::Relaxed);
    let compact = shared.gate.compact.load(Ordering::Relaxed);
    let chat = shared.gate.chat.load(Ordering::Relaxed);
    island::apply_geometry(&app, &pref, collapsed, compact, chat);
}

#[tauri::command]
fn open_url(url: String) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    platform::open_url(&url);
}

/// "Open terminal" opens the working folder in VS Code when `code` is on PATH,
/// and falls back to the file manager otherwise.
#[tauri::command]
fn open_in_vscode(path: Option<String>) -> bool {
    // No shell anywhere near this. The path is a project folder chosen by
    // whoever is using Claude Code, and a shell would happily read `&`, `^`, `%`
    // or `$` in a folder name as syntax. Finding the launcher ourselves and
    // handing the path over as a separate argument keeps it a path.
    let path = path.filter(|p| !p.is_empty());
    // It arrives in a hook payload: only an existing folder, given by its full
    // path, goes any further. `code` would read `--something` as an option, and
    // xdg-open would launch a file with whatever handles its type.
    if let Some(p) = path.as_deref() {
        let p = std::path::Path::new(p);
        if !(p.is_absolute() && p.is_dir()) {
            return false;
        }
    }
    if let Some(code) = platform::find_on_path("code") {
        let mut cmd = Command::new(code);
        if let Some(p) = path.as_deref() {
            cmd.arg(p);
        }
        if platform::no_console(&mut cmd).spawn().is_ok() {
            return true;
        }
    }
    if let Some(p) = path.as_deref() {
        platform::reveal_folder(p);
    }
    false
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

/// Tray → Pause. Paused means paused: the pollers stop talking to the network,
/// not just the island stopping showing things.
#[tauri::command]
fn set_paused(paused: bool) {
    integrations::set_paused(paused);
}

// ── Claude Code hooks ─────────────────────────────────────────────────────────

#[tauri::command]
fn hooks_status() -> HookStatus {
    hooks::status()
}

/// Returns the diff the user has to look at before anything is written.
#[tauri::command]
fn hooks_preview(install: bool) -> Result<HookPreview, String> {
    hooks::preview(install)
}

/// Only ever called from an explicit click in the settings window.
#[tauri::command]
fn hooks_apply(
    app: AppHandle,
    shared: State<Shared>,
    install: bool,
    fingerprint: String,
) -> Result<String, String> {
    // The fingerprint comes from the preview the user actually looked at, so a
    // settings.json that changed in between is refused rather than overwritten.
    let backup = hooks::write(install, &fingerprint)?;
    let updated = {
        let mut current = shared.settings.lock().unwrap();
        current.hooks_installed = install;
        let _ = settings::save(&current);
        current.clone()
    };
    let _ = app.emit("settings-changed", updated);
    Ok(backup)
}

// ── opencode integration ──────────────────────────────────────────────────────

#[tauri::command]
fn opencode_status() -> opencode::OpenCodeStatus {
    opencode::status()
}

/// Copies Coucou's opencode plugin into the user's opencode config.
#[tauri::command]
fn opencode_install(app: AppHandle) -> Result<String, String> {
    let result = opencode::install();
    let _ = app.emit("opencode-changed", opencode::status().installed);
    result
}

/// Removes Coucou's plugin — only when it is still ours.
#[tauri::command]
fn opencode_uninstall(app: AppHandle) -> Result<(), String> {
    let result = opencode::uninstall();
    let _ = app.emit("opencode-changed", opencode::status().installed);
    result
}

/// Recent opencode sessions, newest first, for the island's sessions card.
#[tauri::command]
async fn opencode_sessions(limit: u16) -> Result<Vec<opencode::OpencodeSession>, String> {
    tauri::async_runtime::spawn_blocking(move || opencode::sessions(limit))
        .await
        .map_err(|e| e.to_string())?
}

/// Resumes a session: focuses its window, or opens it in a new terminal.
#[tauri::command]
async fn opencode_continue(session_id: String, directory: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || opencode::continue_session(&session_id, &directory))
        .await
        .map_err(|e| e.to_string())?
}

/// Answers in a session from the island chat, no terminal involved.
/// `model` is the bound `provider/model` for the opencode agent, or empty for the
/// session's own default — Coucou never changes the model of a session the user
/// already started unless a binding says so.
#[tauri::command]
async fn opencode_run(
    session_id: String,
    directory: String,
    message: String,
    model: Option<String>,
) -> Result<String, String> {
    let model = model.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        opencode::run(&session_id, &directory, &message, &model)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn approval_decision(app: AppHandle, request_id: String, decision: String) {
    pipe::answer(&app, &request_id, &decision);
}

/// The island has the card on screen, so the long wait for a human may begin.
/// Until this arrives the relay only waits a few hundred milliseconds, which is
/// what stops a paused or unresponsive island from freezing Claude Code.
#[tauri::command]
fn approval_ack(app: AppHandle, request_id: String) {
    pipe::acknowledge(&app, &request_id);
}

/// Nobody can act on this request — the island is paused, or another card is
/// already up. Claude Code falls back to asking in the terminal immediately.
#[tauri::command]
fn approval_decline(app: AppHandle, request_id: String) {
    pipe::decline(&app, &request_id);
}

// ── Chat, files and secrets ───────────────────────────────────────────────────

/// One chat turn. The API key and any file bytes stay on the Rust side.
#[tauri::command]
async fn chat_send(
    shared: State<'_, Shared>,
    chat: State<'_, Chat>,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let (provider, model) = {
        let s = shared.settings.lock().unwrap();
        (s.provider.clone(), s.model.clone())
    };
    let settings = shared.settings.lock().unwrap().clone();
    chat::send(
        &chat,
        &settings,
        &provider,
        &model,
        query,
        context,
        erp_is_configured(),
    )
    .await
}

/// True when every SAP credential is stored. Decided here from the keychain so
/// the chat prompt cannot be spoofed from the front end.
fn erp_is_configured() -> bool {
    const SAP_KEYS: [&str; 4] = ["sapb1-url", "sapb1-company", "sapb1-user", "sapb1-password"];
    SAP_KEYS.iter().all(|k| secrets::present(k))
}

#[tauri::command]
fn chat_reset(chat: State<Chat>) {
    chat.reset();
}

// ── Models and backends ───────────────────────────────────────────────────────

/// The backends the user has saved, with `has_key` computed in Rust so the key
/// itself never crosses IPC.
#[tauri::command]
fn provider_configs(shared: State<'_, Shared>) -> Vec<providers::ConfigInfo> {
    shared
        .settings
        .lock()
        .unwrap()
        .providers
        .iter()
        .map(providers::config_info)
        .collect()
}

/// The read-only catalogue offered when adding a backend.
#[tauri::command]
fn provider_presets() -> Vec<providers::PresetInfo> {
    providers::all_presets()
}

/// Adds a backend from a preset id or a free-form name, and returns its slug.
/// Used by the add-backend dialog.
#[tauri::command]
fn provider_add(
    shared: State<'_, Shared>,
    preset: Option<String>,
    name: Option<String>,
    base_url: Option<String>,
) -> Result<String, String> {
    let mut settings = shared.settings.lock().unwrap();
    let id = match preset.as_deref() {
        // A preset already exists for that id (a second gateway on the same API,
        // or a local runtime that needs no key): keep it, just point it elsewhere.
        Some(preset_id) if settings.provider_config(preset_id).is_some() => {
            if let Some(url) = base_url {
                if !url.trim().is_empty() {
                    if let Some(cfg) = settings.provider_config_mut(preset_id) {
                        cfg.base_url = url.trim_end_matches('/').to_string();
                    }
                }
            }
            preset_id.to_string()
        }
        Some(preset_id) => {
            let preset = providers::find(preset_id)
                .ok_or_else(|| format!("Unknown backend {preset_id}"))?;
            let taken: Vec<String> = settings.providers.iter().map(|p| p.id.clone()).collect();
            let id = providers::unique_slug(preset_id, &taken);
            let mut cfg = preset.config();
            cfg.id = id.clone();
            cfg.key_ref = format!("provider-{id}");
            if let Some(url) = base_url {
                if !url.trim().is_empty() {
                    cfg.base_url = url.trim_end_matches('/').to_string();
                }
            }
            settings.providers.push(cfg);
            id
        }
        None => settings
            .add_provider(name.as_deref().unwrap_or_default(), providers::API_OPENAI, &base_url.unwrap_or_default())
            .ok_or("Give the backend a name.")?,
    };
    settings::save(&settings).map_err(|e| e.to_string())?;
    Ok(id)
}

/// Saves an edited backend: name, endpoint, dialect, default model.
#[tauri::command]
fn provider_update(shared: State<'_, Shared>, id: String, patch: ProviderConfig) -> Result<(), String> {
    let mut settings = shared.settings.lock().unwrap();
    let cfg = settings
        .provider_config_mut(&id)
        .ok_or_else(|| format!("Unknown backend {id}"))?;
    cfg.name = patch.name;
    cfg.style = patch.style;
    cfg.base_url = patch.base_url.trim_end_matches('/').to_string();
    cfg.default_model = patch.default_model;
    cfg.key_required = patch.key_required;
    // id, key_ref and built_in are deliberately not patchable: the first is the
    // lookup key, the second points at a stored secret, the third guards deletion.
    settings::save(&settings).map_err(|e| e.to_string())
}

/// Removes a user backend and every binding that pointed at it.
#[tauri::command]
fn provider_remove(shared: State<'_, Shared>, id: String) -> Result<(), String> {
    let mut settings = shared.settings.lock().unwrap();
    if !settings.remove_provider(&id) {
        return Err(if providers::find(&id).is_some() {
            format!("{} is built in and cannot be removed.", settings.provider_config(&id).map(|c| c.name.clone()).unwrap_or(id))
        } else {
            format!("Unknown backend {id}")
        });
    }
    models::forget(&id);
    settings::save(&settings).map_err(|e| e.to_string())
}

/// Copies a backend under a new name, with a fresh empty key.
#[tauri::command]
fn provider_duplicate(shared: State<'_, Shared>, id: String, name: Option<String>) -> Result<String, String> {
    let mut settings = shared.settings.lock().unwrap();
    let new_id = settings
        .duplicate_provider(&id, name.as_deref().unwrap_or_default())
        .ok_or_else(|| format!("Unknown backend {id}"))?;
    settings::save(&settings).map_err(|e| e.to_string())?;
    Ok(new_id)
}

/// Every cached model, pinned first, for the models page.
#[tauri::command]
fn model_catalog(shared: State<'_, Shared>) -> Vec<models::ModelEntry> {
    let settings = shared.settings.lock().unwrap();
    models::catalog(&settings)
}

/// When a backend's list was last fetched, for the "updated" label.
#[tauri::command]
fn model_freshness(provider: String) -> Option<String> {
    models::fetched_at(&provider)
}

/// Fetches a backend's model list live and caches it.
#[tauri::command]
async fn model_refresh(
    shared: State<'_, Shared>,
    provider_id: String,
) -> Result<Vec<models::ModelEntry>, String> {
    let settings = shared.settings.lock().unwrap().clone();
    models::refresh(&settings, &provider_id).await
}

/// Stars or unstars a model. Pins live in settings, not the cache.
#[tauri::command]
fn model_pin(shared: State<'_, Shared>, provider: String, id: String, on: bool) -> Result<(), String> {
    let mut settings = shared.settings.lock().unwrap();
    models::set_pin(&mut settings, &provider, &id, on)?;
    settings::save(&settings).map_err(|e| e.to_string())
}

/// Makes a catalogued model the default for its backend, and points the chat
/// picker at it when that backend is the one chat uses.
#[tauri::command]
fn model_set_default(shared: State<'_, Shared>, provider: String, id: String) -> Result<(), String> {
    let mut settings = shared.settings.lock().unwrap();
    models::set_default_model(&mut settings, &provider, &id)?;
    if settings.provider == provider {
        settings.model = id;
    }
    settings::save(&settings).map_err(|e| e.to_string())
}

/// Binds one agent to a backend and model. Pass an empty model for "the backend
/// default". Only Coucou-originated requests use this; an external Claude Code or
/// VS Code session owns its own model choice, so its binding is display-only.
#[tauri::command]
fn agent_bind(
    shared: State<'_, Shared>,
    agent_id: String,
    provider: String,
    model: Option<String>,
) -> Result<(), String> {
    let mut settings = shared.settings.lock().unwrap();
    if agent_id.is_empty() {
        return Err("Missing agent.".into());
    }
    if settings.provider_config(&provider).is_none() {
        return Err(format!("Unknown backend {provider}"));
    }
    let model = model.unwrap_or_default();
    match settings.agents.iter_mut().find(|a| a.agent_id == agent_id) {
        Some(b) => {
            b.provider = provider;
            b.model = model;
        }
        None => settings.agents.push(AgentBinding {
            agent_id,
            provider,
            model,
        }),
    }
    settings::save(&settings).map_err(|e| e.to_string())
}

/// Live model list for one backend, for a picker.
#[tauri::command]
async fn provider_models(shared: State<'_, Shared>, provider_id: String) -> Result<Vec<Value>, String> {
    let settings = shared.settings.lock().unwrap().clone();
    chat::list_models(&settings, &provider_id).await
}

/// Copies a dropped file into the inbox and reports its name back.
#[tauri::command]
fn ingest_file(path: String) -> Result<DroppedFile, String> {
    files::ingest(&path)
}

/// The island may only ask whether a key exists — never read it.
#[tauri::command]
fn secret_present(key: String) -> bool {
    secrets::present(&key)
}

#[tauri::command]
fn secret_set(key: String, value: String) -> Result<(), String> {
    secrets::set(&key, &value)
}

#[tauri::command]
fn secret_clear(key: String) -> Result<(), String> {
    secrets::clear(&key)
}

/// Opens the configured n8n instance — the URL lives in the Credential Manager.
#[tauri::command]
fn open_n8n() {
    if let Some(url) = secrets::get("n8n-url") {
        open_url(url);
    }
}

/// Refresh buttons in the integration cards.
#[tauri::command]
async fn refresh_integration(app: AppHandle, id: String) {
    integrations::poll_once(app, &id).await;
}

// ── SAP Business One ──────────────────────────────────────────────────────────

/// Connects to Business One and reports which report entity sets and fields the
/// live server actually has. Credentials come from the Credential Manager.
#[tauri::command]
async fn sap_b1_probe() -> Result<sapb1::Probe, String> {
    sapb1::probe(&sapb1::credentials_from_secrets()?).await
}

/// Answers a question about the ERP from the island chat. The planner reads the
/// SAP agent's backend binding from settings, so it answers conversationally
/// until the user asks for a figure.
#[tauri::command]
async fn sap_b1_ask(shared: State<'_, Shared>, question: String) -> Result<sapb1::Answer, String> {
    let settings = shared.settings.lock().unwrap().clone();
    sapb1::ask::ask(&sapb1::credentials_from_secrets()?, &settings, &question).await
}

/// Clears a pending clarifying turn — called when the chat leaves the ERP or the
/// island closes, so stale context cannot answer a later question.
#[tauri::command]
fn sap_b1_reset() {
    sapb1::ask::clear_pending();
}

/// Lets the island write to the same log as the Rust side.
#[tauri::command]
fn log_line(message: String) {
    log::line(format!("ui  {message}"));
}

// ── Settings window ───────────────────────────────────────────────────────────

/// WebView2 allows exactly one browser environment per app, and its options are
/// fixed by whichever webview is created first. Every window must therefore ask
/// for the *same* arguments as the island (see `additionalBrowserArgs` in
/// tauri.conf.json) — a mismatch makes the second window come up blank, with no
/// error anywhere.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required";

/// In a dev build the pages are served by Vite, so the second window needs the
/// absolute dev URL; a bundled build resolves it inside the app bundle.
fn settings_page_url(app: &AppHandle) -> WebviewUrl {
    #[cfg(dev)]
    if let Some(mut base) = app.config().build.dev_url.clone() {
        base.set_path("/settings.html");
        return WebviewUrl::External(base);
    }
    let _ = app;
    WebviewUrl::App("settings.html".into())
}

/// The settings window is created hidden at launch and only ever shown and
/// hidden afterwards. A WebView2 window created later — on the main thread or
/// not — silently comes up blank in this app, so the window that works is the
/// one that exists before the island's webview does.
fn create_settings_window(app: &AppHandle) {
    let url = settings_page_url(app);
    match WebviewWindowBuilder::new(app, "settings", url)
        .additional_browser_args(BROWSER_ARGS)
        .title("Settings — Coucou")
        .inner_size(940.0, 640.0)
        .min_inner_size(760.0, 480.0)
        .resizable(true)
        .visible(false)
        .center()
        .build()
    {
        Ok(win) => {
            // Closing it must only hide it, or it could never be reopened.
            let hidden = win.clone();
            win.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = hidden.hide();
                }
            });
        }
        Err(err) => log::line(format!("settings window failed: {err}")),
    }
}

pub fn show_settings_window(app: &AppHandle) {
    let Some(win) = app.get_webview_window("settings") else {
        log::line("settings window missing");
        return;
    };
    let _ = win.unminimize();
    let _ = win.show();
    let _ = win.set_focus();
}

/// Opens the settings window on a given page. `page` is one of the sidebar ids,
/// or empty to keep the current page. The page is sent as an event so the
/// pre-created window does not reload.
#[tauri::command]
fn open_settings_window(app: AppHandle, page: Option<String>) {
    if let Some(page) = page {
        if !page.is_empty() {
            let _ = app.emit_to("settings", "settings-open-page", page);
        }
    }
    show_settings_window(&app);
}

pub fn run() {
    platform::prepare_environment();
    let loaded = settings::load();
    let gate = Arc::new(PollGate::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            let _ = app.emit_to(island::WINDOW_LABEL, "tray", "open".to_string());
        }))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .manage(Shared {
            settings: Mutex::new(loaded.clone()),
            gate: gate.clone(),
        })
        .manage(Pending::default())
        .manage(Chat::default())
        .invoke_handler(tauri::generate_handler![
            boot,
            save_settings,
            set_collapsed,
            set_compact,
            set_chat_expanded,
            set_island_rect,
            focus_window,
            reposition,
            open_url,
            open_in_vscode,
            quit_app,
            hooks_status,
            hooks_preview,
            hooks_apply,
            opencode_status,
            opencode_install,
            opencode_uninstall,
            opencode_sessions,
            opencode_continue,
            opencode_run,
            approval_decision,
            approval_ack,
            approval_decline,
            log_line,
            chat_send,
            chat_reset,
            provider_configs,
            provider_presets,
            provider_add,
            provider_update,
            provider_remove,
            provider_duplicate,
            provider_models,
            model_catalog,
            model_freshness,
            model_refresh,
            model_pin,
            model_set_default,
            agent_bind,
            ingest_file,
            secret_present,
            secret_set,
            secret_clear,
            refresh_integration,
            open_n8n,
            sap_b1_probe,
            sap_b1_ask,
            sap_b1_reset,
            open_settings_window,
            set_paused,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            tray::build(&handle)?;
            // Before the island: see create_settings_window.
            create_settings_window(&handle);

            if let Some(win) = island::window(&handle) {
                platform::make_non_activating(&win);
                island::apply_geometry(&handle, &loaded.screen, false, false, false);
                let _ = win.show();
            }
            gate.collapsed.store(false, Ordering::Relaxed);
            // Nothing drawn yet, so nothing takes the mouse until the page
            // reports the island's shape.
            if !platform::CURSOR_POLL {
                island::refresh_click_through(&handle, &gate);
            }
            gate.set_active(true);
            island::spawn_cursor_poll(handle.clone(), gate.clone());

            log::line(format!("--- Coucou {} started ---", env!("CARGO_PKG_VERSION")));
            hooks::ensure_hook_exe(&handle);
            pipe::start(handle.clone());
            integrations::start(handle.clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Coucou");
}
