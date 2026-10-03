// Thin wrapper over the Tauri commands/events. Every call is a no-op when the
// page is opened in a plain browser, so the island can be iterated on with
// `npm run dev` alone.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { ProviderConfig, Settings } from "./state";

export const IS_TAURI =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T | null> {
  if (!IS_TAURI) return null;
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    console.error(`[coucou] ${cmd} failed`, err);
    return null;
  }
}

export interface BootInfo {
  settings: Settings;
  /** Logical screen rect of the monitor the island lives on. */
  screen: { x: number; y: number; width: number; height: number; scale: number };
  version: string;
  hookPath: string;
  /** True when Coucou's opencode plugin is installed. */
  opencodeInstalled: boolean;
  /** False where the OS has no global cursor (Wayland): see Island.followPageCursor. */
  cursorPoll: boolean;
}

export const Bridge = {
  boot: () => call<BootInfo>("boot"),

  saveSettings: (settings: Settings) => call<void>("save_settings", { settings }),

  /** Shrink the window down to the invisible wake strip (hidden) or back to full. */
  setCollapsed: (collapsed: boolean) => call<void>("set_collapsed", { collapsed }),

  /** Shrink to a single centred pill when no pill row is shown. */
  setCompact: (compact: boolean) => call<void>("set_compact", { compact }),

  /** Grow the window taller while the chat is open. */
  setChatExpanded: (expanded: boolean) => call<void>("set_chat_expanded", { expanded }),

  /**
   * Pushes the island shape in window coordinates. Rust flips click-through from
   * its own cursor poll, so the flag is never a frame behind a click.
   */
  setIslandRect: (x: number, y: number, width: number, height: number) =>
    call<void>("set_island_rect", { x, y, width, height }),

  /** Give the window keyboard focus (chat field) and take it away again. */
  focusWindow: (focused: boolean) => call<void>("focus_window", { focused }),

  reposition: () => call<void>("reposition"),

  openUrl: (url: string) => call<void>("open_url", { url }),

  /** "Open terminal" → opens the folder in VS Code when `code` is on PATH. */
  openInVSCode: (path: string | null) => call<boolean>("open_in_vscode", { path }),

  quit: () => call<void>("quit_app"),

  openSettingsWindow: (page?: string) => call<void>("open_settings_window", { page }),

  /** Writes to %LOCALAPPDATA%\Coucou\coucou.log, next to the Rust lines. */
  log: (message: string) => call<void>("log_line", { message }),

  // ── Claude Code hooks ─────────────────────────────────────────────────────
  hooksStatus: () => call<HookStatus>("hooks_status"),
  /** Diff to show before anything is written. `install: false` previews removal. */
  hooksPreview: (install: boolean) => callOrThrow<HookPreview>("hooks_preview", { install }),
  /**
   * Writes ~/.claude/settings.json — only ever after an explicit click, and only
   * when the file still matches the preview the user looked at.
   */
  hooksApply: (install: boolean, fingerprint: string) =>
    callOrThrow<string>("hooks_apply", { install, fingerprint }),

  // ── opencode integration ──────────────────────────────────────────────────
  openCodeStatus: () => call<OpenCodeStatus>("opencode_status"),
  /** Copies Coucou's plugin into the opencode config; returns a backup path. */
  openCodeInstall: () => callOrThrow<string>("opencode_install"),
  openCodeUninstall: () => callOrThrow<void>("opencode_uninstall"),
  /** Recent sessions, newest first, for the sessions card. */
  opencodeSessions: (limit = 10) => callOrThrow<OpencodeSession[]>("opencode_sessions", { limit }),
  /** Focus a running session's window, or open it in a new terminal. */
  opencodeContinue: (sessionId: string, directory: string) =>
    callOrThrow<boolean>("opencode_continue", { sessionId, directory }),
  /** Answers in an existing session: `opencode run -s <id> <message>`. */
  opencodeRun: (sessionId: string, directory: string, message: string, model?: string) =>
    callOrThrow<string>("opencode_run", { sessionId, directory, message, model }),

  approvalDecision: (requestId: string, decision: "allow" | "deny") =>
    call<void>("approval_decision", { requestId, decision }),
  /** "The card is up" — until this lands the relay only waits a moment. */
  approvalAck: (requestId: string) => call<void>("approval_ack", { requestId }),
  /** "Nobody can act on this" — Claude Code asks in the terminal right away. */
  approvalDecline: (requestId: string) => call<void>("approval_decline", { requestId }),

  // ── Chat, files, secrets ──────────────────────────────────────────────────
  /** One chat turn. The API key and any file bytes never leave Rust. */
  chatSend: (query: string, context: ChatContext | null) =>
    callOrThrow<{ text: string }>("chat_send", { query, context }),
  chatReset: () => call<void>("chat_reset"),

  // ── Backends and models ───────────────────────────────────────────────────
  /** The backends the user has saved, with `hasKey` computed in Rust. */
  providerConfigs: () => call<ProviderConfigInfo[]>("provider_configs"),
  /** The read-only catalogue for adding a backend. */
  providerPresets: () => call<ProviderPreset[]>("provider_presets"),
  /** Adds a backend from a preset or a free-form name; returns its slug. */
  providerAdd: (args: { preset?: string; name?: string; baseUrl?: string }) =>
    callOrThrow<string>("provider_add", args),
  providerUpdate: (id: string, patch: ProviderConfig) =>
    callOrThrow<void>("provider_update", { id, patch }),
  providerRemove: (id: string) => callOrThrow<void>("provider_remove", { id }),
  providerDuplicate: (id: string, name?: string) =>
    callOrThrow<string>("provider_duplicate", { id, name }),
  /** Live model list for one backend. */
  providerModels: (providerId: string) =>
    callOrThrow<ModelInfo[]>("provider_models", { providerId }),
  /** Every cached model, pinned first, for the models page. */
  modelCatalog: () => call<ModelEntry[]>("model_catalog"),
  /** When a backend's list was last fetched. */
  modelFreshness: (provider: string) => call<string | null>("model_freshness", { provider }),
  /** Fetches a backend's model list live and caches it. */
  modelRefresh: (providerId: string) =>
    callOrThrow<ModelEntry[]>("model_refresh", { providerId }),
  modelPin: (provider: string, id: string, on: boolean) =>
    callOrThrow<void>("model_pin", { provider, id, on }),
  modelSetDefault: (provider: string, id: string) =>
    callOrThrow<void>("model_set_default", { provider, id }),
  /** Binds one agent to a backend and model. */
  agentBind: (agentId: string, provider: string, model: string | null) =>
    callOrThrow<void>("agent_bind", { agentId, provider, model }),

  /** Copies a dropped file into the inbox. */
  ingestFile: (path: string) => callOrThrow<DroppedFile>("ingest_file", { path }),
  /** Only ever tells you whether a key exists — never its value. */
  secretPresent: (key: string) => call<boolean>("secret_present", { key }),
  secretSet: (key: string, value: string) => callOrThrow<void>("secret_set", { key, value }),
  secretClear: (key: string) => callOrThrow<void>("secret_clear", { key }),

  // ── Integrations ──────────────────────────────────────────────────────────
  refreshIntegration: (id: string) => call<void>("refresh_integration", { id }),
  /** Opens the configured n8n instance in the browser. */
  openN8n: () => call<void>("open_n8n"),
  /**
   * Connects to SAP Business One and reports which report entity sets and
   * fields the live server has. Credentials live in the Credential Manager.
   */
  sapB1Probe: () => callOrThrow<SapB1Probe>("sap_b1_probe"),
  /** Answers a question about the ERP from the island chat. `history` is the
   *  conversation so far, so the planner stays aware and does not re-ask. */
  sapB1Ask: (question: string, history: ChatTurn[]) =>
    callOrThrow<SapB1Answer>("sap_b1_ask", { question, history }),

  /** Tray → Pause. Stops the integration pollers, not just the island. */
  setPaused: (paused: boolean) => call<void>("set_paused", { paused }),
};

/** One report entity set, checked against the live Service Layer. */
export interface SapB1SetProbe {
  entitySet: string;
  present: boolean;
  entityType: string | null;
  fieldCount: number;
  /** Report fields the server does not have. Empty is the good case. */
  missingFields: string[];
}

export interface SapB1Probe {
  entitySetCount: number;
  typeCount: number;
  sets: SapB1SetProbe[];
  /** True when every report set and every report field exists live. */
  ready: boolean;
}

export interface SapB1AnswerRow {
  label: string;
  value: number;
}

/** One prior turn of a conversation, sent to the ERP planner. */
export interface ChatTurn {
  role: "user" | "assistant";
  content: string;
}

/** The ERP's answer to a chat question. */
export interface SapB1Answer {
  title: string;
  /** A readable block, ready for the chat log. */
  text: string;
  rows: SapB1AnswerRow[];
  total: number | null;
  /** True when a row cap cut the data set, so the figure is a lower bound. */
  partial: boolean;
  /** Which query produced this, so the figure can be trusted or challenged. */
  source: string;
  /** Tappable follow-ups. Empty when the question was understood. */
  suggestions: string[];
  /** "result" | "clarify" | "answer" | "help" — how the front end renders it. */
  kind: string;
  /** The "what I'll do" bubble, shown before a result. */
  plan: string | null;
  /** The question the assistant needs answered before it can run. */
  clarifyingQuestion: string | null;
}

export interface OpenCodeStatus {
  installed: boolean;
  /** False when the installed plugin differs from the one this build ships. */
  upToDate: boolean;
  pluginPath: string;
  configDir: string;
}

/** One opencode session from `opencode session list --format json`. */
export interface OpencodeSession {
  id: string;
  title: string;
  directory: string;
  updated: number;
  created: number;
  /** True when the plugin reported this session over the relay right now. */
  live: boolean;
}

export interface ProviderInfo {
  id: string;
  name: string;
  accent: string;
  defaultModel: string;
  /** Credential Manager key that holds this provider's API key. */
  key: string;
  keyRequired: boolean;
  /** "anthropic" or "openai" — the API dialect. */
  style: string;
}

/** A saved backend, as the settings window sees it. */
export interface ProviderConfigInfo {
  id: string;
  name: string;
  accent: string;
  style: string;
  baseUrl: string;
  defaultModel: string;
  keyRequired: boolean;
  builtIn: boolean;
  /** Computed in Rust so the key never crosses IPC. */
  hasKey: boolean;
  keyRef: string;
  pinnedModels: string[];
}

/** The read-only catalogue for adding a backend. */
export interface ProviderPreset {
  id: string;
  name: string;
  accent: string;
  style: string;
  baseUrl: string;
  keyRef: string;
  defaultModel: string;
  keyRequired: boolean;
  note: string;
}

/** One row of the models table. */
export interface ModelEntry {
  id: string;
  label: string;
  provider: string;
  context: number | null;
  costIn: number | null;
  costOut: number | null;
  /** true = known free, false = known paid, null = the source did not say. */
  free: boolean | null;
}

export interface ModelInfo {
  id: string;
  label: string;
}

export interface IntegrationUpdate {
  id: string;
  data: Record<string, unknown>;
  error: string | null;
  event: { success: boolean; label: string; detail: string | null } | null;
}

export type ChatContext =
  | { kind: "file"; name: string; path: string }
  | { kind: "window"; appName: string; title: string; url?: string };

export interface DroppedFile {
  name: string;
  path: string;
  size: number;
}

export interface HookStatus {
  installed: boolean;
  settingsPath: string;
  hookPath: string;
  hookReady: boolean;
}

export interface HookPreview {
  diff: string;
  backup: string;
  settingsPath: string;
  /** Hand back to hooksApply so only the reviewed diff is ever written. */
  fingerprint: string;
}

/** Same as `call`, but surfaces the error so the UI can show what went wrong. */
async function callOrThrow<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!IS_TAURI) throw new Error("not running inside Coucou");
  return invoke<T>(cmd, args);
}

export type BridgeEvent =
  | { name: "cursor"; payload: { x: number; y: number } }
  | { name: "tray"; payload: string }
  | { name: "hook"; payload: Record<string, unknown> }
  | { name: "screen-changed"; payload: null };

export interface DragDropPayload {
  type: "enter" | "over" | "drop" | "leave";
  paths?: string[];
}

/** Files dragged onto the island. Only reaches us when the window takes the mouse. */
export async function onDragDrop(handler: (e: DragDropPayload) => void) {
  if (!IS_TAURI) return () => {};
  return getCurrentWebview().onDragDropEvent((event) => {
    handler(event.payload as DragDropPayload);
  });
}

export async function onEvent<T>(name: string, handler: (payload: T) => void) {
  if (!IS_TAURI) return () => {};
  return listen<T>(name, (e) => handler(e.payload));
}
