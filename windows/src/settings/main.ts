// Settings window — sidebar shell with one page at a time.
// Anything that writes to disk is confirmed here; API keys never leave Rust.

import "./settings.css";
import {
  Bridge, onEvent,
  type HookStatus,
  type ModelEntry,
  type OpenCodeStatus,
  type ProviderConfigInfo,
  type ProviderPreset,
} from "../core/bridge";
import { DEFAULT_SETTINGS, type ProviderConfig, type Settings } from "../core/state";
import { h, clear } from "../views/dom";

type PageId = "general" | "agents" | "models" | "integrations" | "appearance" | "advanced";

const PAGES: { id: PageId; label: string }[] = [
  { id: "general", label: "General" },
  { id: "agents", label: "Agents" },
  { id: "models", label: "Models" },
  { id: "integrations", label: "Integrations" },
  { id: "appearance", label: "Appearance" },
  { id: "advanced", label: "Advanced" },
];

let settings: Settings = { ...DEFAULT_SETTINGS };
let version = "";
let page: PageId = "general";
let opencode: OpenCodeStatus = { installed: false, upToDate: false, pluginPath: "", configDir: "" };
let hookStatus: HookStatus = { installed: false, settingsPath: "", hookPath: "", hookReady: false };
let integrationsPaused = false;

const root = document.getElementById("settings-root")!;
const pageHost = h("div", { class: "settings-page" });

async function save() {
  await Bridge.saveSettings(settings);
}

// ── Reusable bits ─────────────────────────────────────────────────────────────

function toggle(on: boolean, onChange: (v: boolean) => void): HTMLElement {
  const el = h("button", { class: on ? "switch on" : "switch", "aria-pressed": on });
  el.addEventListener("click", () => {
    const next = !el.classList.contains("on");
    el.classList.toggle("on", next);
    onChange(next);
  });
  return el;
}

/**
 * A status dot. "off" is neutral gray, not red: a service the user simply has
 * not set up (Claude Code while they use SAP) must never look like an error.
 * Red is reserved for a thing that is expected to work but is broken.
 */
type DotState = "ok" | "off" | "error";
function statusDot(state: DotState | boolean): HTMLElement {
  const s: DotState = typeof state === "boolean" ? (state ? "ok" : "off") : state;
  const color = s === "ok" ? "#22c55e" : s === "error" ? "#f4505e" : "#6b7079";
  return h("i", { class: "dot", style: `background:${color}` });
}

function modelDisplay(m: ModelEntry): string {
  return m.label || m.id;
}

function row(label: string, ...controls: (HTMLElement | string)[]): HTMLElement {
  return h("div", { class: "row" }, h("label", { text: label }), ...controls);
}

function hint(text: string, color?: string): HTMLElement {
  const el = h("span", { class: "hint", text });
  if (color) el.style.color = color;
  return el;
}

function group(title: string, ...children: (HTMLElement | string)[]): HTMLElement {
  return h("section", { class: "group" }, h("h3", { text: title }), ...children);
}

function renderDiff(text: string): HTMLElement {
  const box = h("div", { class: "diff" });
  for (const line of text.split("\n")) {
    const cls = line.startsWith("+") ? "add" : line.startsWith("-") ? "del" : "ctx";
    box.append(h("div", { class: cls, text: line }));
  }
  return box;
}

// ── General ───────────────────────────────────────────────────────────────────

function generalPage(): HTMLElement {
  const volume = h("input", {
    type: "range", min: "0", max: "0.2", step: "0.005", value: String(settings.soundVolume),
  }) as HTMLInputElement;
  volume.addEventListener("input", () => { settings.soundVolume = Number(volume.value); void save(); });

  const autoClose = h("input", {
    type: "number", min: "5", max: "120", step: "1", value: String(Math.round(settings.autoCloseInterval)),
    style: "width:72px",
  }) as HTMLInputElement;
  autoClose.addEventListener("change", () => {
    settings.autoCloseInterval = Math.max(5, Math.min(120, Number(autoClose.value) || 15));
    autoClose.value = String(settings.autoCloseInterval);
    void save();
  });

  const screen = h("select", {}) as HTMLSelectElement;
  screen.append(h("option", { value: "primary", text: "Main display" }), h("option", { value: "cursor", text: "Display under the cursor" }));
  screen.value = settings.screen;
  screen.addEventListener("change", () => { settings.screen = screen.value as Settings["screen"]; void save(); });

  return h("div", { class: "page" },
    h("h2", { text: "General" }),
    group("Sounds",
      row("Sound", toggle(settings.soundEnabled, (v) => { settings.soundEnabled = v; void save(); }), volume),
    ),
    group("Island",
      row("Auto-close", autoClose, hint("seconds after you leave")),
      row("Lives on", screen),
    ),
    group("Startup",
      row("Launch at login", toggle(settings.autostart, (v) => { settings.autostart = v; void save(); })),
    ),
  );
}

// ── Agents ────────────────────────────────────────────────────────────────────

function agentsPage(): HTMLElement {
  const body = h("div", { class: "page" });
  body.append(h("h2", { text: "Agents" }), hint("Which backend each pill uses when it answers through Coucou. Claude Code and VS Code own their own model, so their binding is display-only."));

  const host = h("div", { style: "display:flex;flex-direction:column;gap:12px" });
  body.append(host);

const agents: { id: string; name: string; pill: boolean }[] = [
    { id: "integration_sapb1", name: "SAP Harness", pill: true },
    { id: "agent_opencode", name: "Opencode", pill: true },
    { id: "integration_claude", name: "Claude Code (display only)", pill: true },
    { id: "chat", name: "Chat", pill: false },
  ];

  function draw() {
    clear(host);
    for (const agent of agents) {
      const bind = settings.agents.find((a) => a.agentId === agent.id)
        ?? { agentId: agent.id, provider: settings.provider, model: settings.model };
      const providerSelect = h("select", {}) as HTMLSelectElement;
      for (const p of settings.providers) providerSelect.append(h("option", { value: p.id, text: p.name }));
      providerSelect.value = bind.provider;

      const modelSelect = h("select", {}) as HTMLSelectElement;
      const defaultOpt = h("option", { value: "", text: "backend default" });
      modelSelect.append(defaultOpt);
      const catalog = modelCatalogCache[bind.provider] ?? [];
      for (const m of catalog) modelSelect.append(h("option", { value: m.id, text: modelDisplay(m) }));
      modelSelect.value = bind.model;

      providerSelect.addEventListener("change", () => {
        setBinding(agent.id, providerSelect.value, "");
      });
      modelSelect.addEventListener("change", () => {
        setBinding(agent.id, providerSelect.value, modelSelect.value);
      });

      const controls: HTMLElement[] = [row("Provider", providerSelect), row("Model", modelSelect)];
      if (agent.pill) {
        // (a) whether the pill is shown at all — the launch default.
        const shown = toggle(settings.features[`pill.${agent.id}`] !== false, (v) => {
          settings.features = { ...settings.features, [`pill.${agent.id}`]: v };
          void save();
        });
        // (b) which pill is focused first.
        const isDefault = settings.defaultAgent === agent.id;
        const defBtn = h("button", { class: isDefault ? "toggle-pill on" : "toggle-pill", text: isDefault ? "Default" : "Make default" });
        defBtn.addEventListener("click", () => {
          settings.defaultAgent = settings.defaultAgent === agent.id ? "" : agent.id;
          void save();
          draw();
        });
        controls.push(row("Show pill", shown), row("On launch", defBtn));
      }

      host.append(group(agent.name, ...controls));
    }
  }

  function setBinding(agentId: string, provider: string, model: string) {
    if (agentId === "chat") {
      settings.provider = provider;
      settings.model = model;
    } else {
      const existing = settings.agents.find((a) => a.agentId === agentId);
      if (existing) { existing.provider = provider; existing.model = model; }
      else settings.agents.push({ agentId, provider, model });
    }
    void save();
    draw();
    void refreshCatalogFor(provider);
  }

  draw();
  for (const p of settings.providers) void refreshCatalogFor(p.id);
  return body;
}

// ── Models ────────────────────────────────────────────────────────────────────

let modelCatalogCache: Record<string, ModelEntry[]> = {};
let providerConfigs: ProviderConfigInfo[] = [];
let loadingProviders = new Set<string>();
/** The models page registers a redraw so an auto-refresh refills the table. */
let redrawCatalog: (() => void) | null = null;

async function loadProviderConfigs() {
  providerConfigs = (await Bridge.providerConfigs()) ?? [];
}

async function refreshCatalogFor(provider: string): Promise<void> {
  loadingProviders.add(provider);
  redrawCatalog?.();
  try {
    const list = (await Bridge.modelRefresh(provider)) ?? [];
    modelCatalogCache[provider] = list;
  } catch {
    // Keep whatever was cached; the models page shows the error next to the row.
  } finally {
    loadingProviders.delete(provider);
  }
  // Without this the table stays "No models yet" until the user changes
  // something, because nothing re-rendered after the fetch.
  redrawCatalog?.();
}

function modelsPage(): HTMLElement {
  const body = h("div", { class: "page" });
  body.append(h("h2", { text: "Models" }), hint("Every backend you have saved, and the models it can answer with. Cost and context come from the catalogue, never guessed from a model name."));

  const layout = h("div", { class: "models-layout" });
  const left = h("div", { class: "providers" });
  const right = h("div", { class: "catalog" });
  layout.append(left, right);
  body.append(layout);

  let selectedProvider = settings.provider;

  function drawProviders() {
    clear(left);
    for (const p of providerConfigs) {
      const item = h("button", { class: `provider-item${p.id === selectedProvider ? " on" : ""}`, onclick: () => {
        selectedProvider = p.id;
        drawProviders();
        drawCatalog();
      } },
        h("i", { class: "dot", style: `background:${p.accent}` }),
        h("span", { text: p.name }),
        h("span", { class: "provider-key", text: p.hasKey ? "●" : "○" }),
      );
      left.append(item);
    }
    const add = h("button", { class: "provider-add", text: "+ Add backend", onclick: () => addProviderDialog() });
    left.append(add);
  }

  function drawCatalog() {
    clear(right);
    const provider = providerConfigs.find((p) => p.id === selectedProvider);
    if (!provider) { right.append(hint("Pick a backend on the left.")); return; }

    right.append(
      h("div", { class: "catalog-head" },
        h("div", { class: "catalog-title" },
          h("span", { text: provider.name }),
          h("span", { class: "hint", text: provider.baseUrl }),
        ),
      ),
    );

    // Key entry.
    const keyRow = h("div", { class: "row" }, h("label", { text: "API key" }));
    const keyInput = h("input", {
      type: "password", placeholder: provider.hasKey ? "••••••••  (stored)" : "paste your key",
      autocomplete: "off", spellcheck: "false", style: "flex:1 1 auto;min-width:0",
    }) as HTMLInputElement;
const keySave = h("button", { class: "primary", text: "Save" });
    const isActive = settings.provider === provider.id;
    const keyHint = provider.hasKey
      ? hint("Stored in the Credential Manager.")
      : hint(
          isActive ? "No key yet — this is the backend the chat uses." : "No key yet.",
          isActive ? "#f5a524" : undefined,
        );
    keySave.addEventListener("click", async () => {
      const v = keyInput.value.trim();
      if (!v) return;
      try {
        await Bridge.secretSet(provider.keyRef, v);
        keyInput.value = "";
        keyHint.textContent = "Stored in the Credential Manager.";
        keyHint.style.color = "";
        provider.hasKey = true;
        await refreshCatalogFor(provider.id);
        drawCatalog();
      } catch (err) {
        keyHint.textContent = `Could not save: ${String(err)}`;
        keyHint.style.color = "#f4505e";
      }
    });
    keyRow.append(keyInput, keySave, keyHint);
    right.append(keyRow);

    // Endpoint (editable only for user backends and custom).
    if (!provider.builtIn || provider.id === "custom") {
      const urlInput = h("input", {
        type: "text", value: provider.baseUrl, placeholder: "https://…/v1",
        autocomplete: "off", spellcheck: "false", style: "flex:1 1 auto;min-width:0",
      }) as HTMLInputElement;
      const urlSave = h("button", { text: "Save" });
      urlSave.addEventListener("click", async () => {
        provider.baseUrl = urlInput.value.trim().replace(/\/+$/, "");
        await Bridge.providerUpdate(provider.id, providerPatch(provider));
        await save();
        await loadProviderConfigs();
        await refreshCatalogFor(provider.id);
        drawCatalog();
      });
      right.append(row("Endpoint", urlInput, urlSave));
    }

    // Default model.
    const defaultRow = h("div", { class: "row" }, h("label", { text: "Default model" }));
    const defaultSelect = h("select", {}) as HTMLSelectElement;
    const defaultOpt = h("option", { value: "", text: "—" });
    defaultSelect.append(defaultOpt);
    for (const m of modelCatalogCache[provider.id] ?? []) defaultSelect.append(h("option", { value: m.id, text: modelDisplay(m) }));
    defaultSelect.value = provider.defaultModel;
    defaultSelect.addEventListener("change", async () => {
      if (!defaultSelect.value) return;
      await Bridge.modelSetDefault(provider.id, defaultSelect.value);
      provider.defaultModel = defaultSelect.value;
      await save();
    });
    defaultRow.append(defaultSelect);
    right.append(defaultRow);

    // Model table.
    const refresh = h("button", { text: "Refresh" });
    const table = h("div", { class: "model-table" });
    const refreshStatus = hint("");
    async function doRefresh() {
      const pid = provider?.id;
      if (!pid) return;
      refreshStatus.textContent = "Loading…";
      refreshStatus.style.color = "";
      try {
        const list = (await Bridge.modelRefresh(pid)) ?? [];
        modelCatalogCache[pid] = list;
        refreshStatus.textContent = `${list.length} models`;
      } catch (err) {
        refreshStatus.textContent = String(err).replace(/^Error:\s*/, "");
        refreshStatus.style.color = "#f5a524";
      }
      drawCatalog();
    }
    refresh.addEventListener("click", () => void doRefresh());
    right.append(h("div", { class: "row" }, h("label", { text: "Models" }), refresh, refreshStatus));

const list = modelCatalogCache[provider.id] ?? [];
    if (list.length === 0 && loadingProviders.has(provider.id)) {
      table.append(hint("Loading models…"));
    } else if (list.length === 0) {
      table.append(hint("No models yet. Click Refresh to fetch the live list."));
    } else {
      const header = h("div", { class: "model-row head" },
        h("span", { text: "Name" }), h("span", { text: "Context" }), h("span", { text: "Cost" }), h("span", { text: "" }));
      table.append(header);
      for (const m of list) {
        const pinned = provider.pinnedModels.includes(m.id);
        const pinBtn = h("button", { class: "pin", text: pinned ? "★" : "☆", title: pinned ? "Unpin" : "Pin" });
        pinBtn.addEventListener("click", async () => {
          await Bridge.modelPin(provider.id, m.id, !pinned);
          await loadProviderConfigs();
          await save();
          drawCatalog();
        });
        const useBtn = h("button", { class: "use", text: "Default", disabled: provider.defaultModel === m.id });
        useBtn.addEventListener("click", async () => {
          await Bridge.modelSetDefault(provider.id, m.id);
          provider.defaultModel = m.id;
          await save();
          drawCatalog();
        });
        table.append(
          h("div", { class: "model-row" },
            h("span", { class: "name", text: modelDisplay(m) }),
            h("span", { class: "ctx", text: m.context ? `${Math.round(m.context / 1000)}K` : "—" }),
            h("span", { class: "cost", text: costLabel(m) }),
            h("span", { class: "actions" }, pinBtn, useBtn),
          ),
        );
      }
    }
    right.append(table);

    // Delete / duplicate for user backends.
    if (!provider.builtIn) {
      const del = h("button", { class: "danger", text: "Delete backend" });
      del.addEventListener("click", async () => {
        await Bridge.providerRemove(provider.id);
        await loadProviderConfigs();
        await save();
        selectedProvider = settings.provider;
        drawProviders();
        drawCatalog();
      });
      const dup = h("button", { text: "Duplicate" });
      dup.addEventListener("click", async () => {
        const newId = await Bridge.providerDuplicate(provider.id);
        await reloadSettings();
        await loadProviderConfigs();
        selectedProvider = newId ?? selectedProvider;
        drawProviders();
        drawCatalog();
      });
      right.append(h("div", { class: "row" }, dup, del));
    }
  }

  /** Builds the saved-shape patch Rust expects from a rendered config. */
  function providerPatch(p: ProviderConfigInfo): ProviderConfig {
    return {
      id: p.id, name: p.name, style: p.style, baseUrl: p.baseUrl,
      keyRef: p.keyRef, defaultModel: p.defaultModel, keyRequired: p.keyRequired,
      builtIn: p.builtIn, pinnedModels: p.pinnedModels,
    };
  }

  function addProviderDialog() {
    const dialog = h("dialog", { class: "dialog" });
    const close = h("button", { class: "danger", text: "Cancel", onclick: () => dialog.close() });
    const presetSelect = h("select", {}) as HTMLSelectElement;
    presetSelect.append(h("option", { value: "", text: "Blank (any OpenAI-compatible endpoint)" }));
    for (const preset of providerPresetCache) presetSelect.append(h("option", { value: preset.id, text: `${preset.name} — ${preset.note}` }));
    const nameInput = h("input", { type: "text", placeholder: "Name, e.g. My Gateway", autocomplete: "off" }) as HTMLInputElement;
    const urlInput = h("input", { type: "text", placeholder: "Base URL, e.g. http://localhost:11434/v1", autocomplete: "off" }) as HTMLInputElement;
    const add = h("button", { class: "primary", text: "Add" });
    add.addEventListener("click", async () => {
      const preset = presetSelect.value || undefined;
      const id = await Bridge.providerAdd({ preset, name: nameInput.value || undefined, baseUrl: urlInput.value || undefined });
      dialog.close();
      await reloadSettings();
      await loadProviderConfigs();
      selectedProvider = id ?? selectedProvider;
      drawProviders();
      drawCatalog();
    });
    dialog.append(
      h("div", { class: "dialog-title", text: "Add a backend" }),
      row("Preset", presetSelect),
      row("Name", nameInput),
      row("Base URL", urlInput),
      h("div", { class: "row" }, h("span", { style: "flex:1" }), close, add),
    );
    document.body.append(dialog);
    dialog.showModal();
  }

  function costLabel(m: ModelEntry): string {
    if (m.free === true) return "Free";
    if (m.free === false) return m.costIn != null ? `$${m.costIn}/M` : "Paid";
    return "—";
  }

drawProviders();
  drawCatalog();
  redrawCatalog = () => { if (page === "models") drawCatalog(); };
  for (const p of providerConfigs) void refreshCatalogFor(p.id);
  return body;
}

let providerPresetCache: ProviderPreset[] = [];

async function loadProviderPresets() {
  providerPresetCache = (await Bridge.providerPresets()) ?? [];
}

// ── Integrations ──────────────────────────────────────────────────────────────

const OTHER_INTEGRATIONS: { id: string; name: string; color: string; fields: { key: string; label: string; placeholder: string; secret: boolean }[] }[] = [
  { id: "integration_stripe", name: "Stripe", color: "#0570DE", fields: [{ key: "stripe-api-key", label: "Secret key", placeholder: "sk_live_…", secret: true }] },
  { id: "integration_github", name: "GitHub", color: "#F4505E", fields: [{ key: "github-token", label: "Token", placeholder: "ghp_…", secret: true }] },
  { id: "integration_vercel", name: "Vercel", color: "#7C5CFF", fields: [{ key: "vercel-token", label: "Token", placeholder: "…", secret: true }] },
  { id: "integration_n8n", name: "n8n", color: "#F29B38", fields: [{ key: "n8n-url", label: "Instance URL", placeholder: "https://n8n.example.com", secret: false }, { key: "n8n-api-key", label: "API key", placeholder: "…", secret: true }] },
  { id: "integration_resend", name: "Resend", color: "#22C55E", fields: [{ key: "resend-api-key", label: "API key", placeholder: "re_…", secret: true }] },
  { id: "integration_notion", name: "Notion", color: "#8C8C8C", fields: [{ key: "notion-api-key", label: "Integration token", placeholder: "ntn_…", secret: true }] },
  { id: "integration_calcom", name: "Cal.com", color: "#C9956A", fields: [{ key: "calcom-api-key", label: "API key", placeholder: "cal_…", secret: true }] },
];

function integrationsPage(): HTMLElement {
  const body = h("div", { class: "page" });
  body.append(h("h2", { text: "Integrations" }), hint("The switch turns the service on or off; pill visibility is set per pill under Pills shown on the Appearance page."));

  const host = h("div", { style: "display:flex;flex-direction:column;gap:12px" });
  body.append(host);

// Claude Code hooks. Neutral when simply not set up — the user may be on SAP,
  // not VS Code — and red only when installed but broken.
  const hookState: DotState = hookStatus.installed ? (hookStatus.hookReady ? "ok" : "error") : "off";
  host.append(
    h("section", { class: "group" },
      h("h3", { style: "display:flex;align-items:center;gap:8px" },
        statusDot(hookState), h("span", { text: "Claude Code" })),
      hint(hookStatus.installed
        ? (hookStatus.hookReady
          ? "Hooked into your Claude Code sessions. Tool calls, questions and permission requests show in the island."
          : "Hooks are installed but the relay is missing. Reinstall to fix it.")
        : "Not set up. Claude Code hooks are only needed if you use Claude Code or VS Code; SAP Harness works without them."),
      row("settings.json", h("span", { class: "path", text: hookStatus.settingsPath })),
      row("Relay", h("span", { class: "path", text: hookStatus.hookPath }), statusDot(hookState)),
      hookActions(),
    ),
  );

  // Opencode.
  host.append(group("Opencode", opencodeRows()));

  // SAP Harness — credentials live here, not in the pill.
  host.append(sapSection());

  // The other services.
  for (const def of OTHER_INTEGRATIONS) {
    const present: Record<string, boolean> = {};
    for (const f of def.fields) present[f.key] = false;
    void loadPresence(def, present, host);
  }

  return body;
}

function hookActions(): HTMLElement {
  const box = h("div", { class: "row" });
  const install = h("button", { class: "primary", text: hookStatus.installed ? "Reinstall hooks…" : "Install hooks…", onclick: () => showHookPreview(true) });
  if (!hookStatus.hookReady) install.disabled = true;
  const remove = h("button", { class: "danger", text: "Remove", onclick: () => showHookPreview(false) });
  box.append(install, remove);
  return box;
}

async function showHookPreview(install: boolean) {
  try {
    const preview = await Bridge.hooksPreview(install);
    const dialog = h("dialog", { class: "dialog" });
    const apply = h("button", { class: "primary", text: install ? "Apply" : "Remove", onclick: async () => {
      try {
        await Bridge.hooksApply(install, preview.fingerprint);
        dialog.close();
        hookStatus = (await Bridge.hooksStatus()) ?? hookStatus;
        render();
      } catch (err) {
        dialog.append(hint(`Could not apply: ${String(err)}`, "#f4505e"));
      }
    } });
    const cancel = h("button", { text: "Cancel", onclick: () => dialog.close() });
    dialog.append(
      h("div", { class: "dialog-title", text: install ? "Install Claude Code hooks" : "Remove Claude Code hooks" }),
      renderDiff(preview.diff),
      h("div", { class: "row" }, h("span", { style: "flex:1" }), cancel, apply),
    );
    document.body.append(dialog);
    dialog.showModal();
  } catch (err) {
    // show in note? just console
    console.error(err);
  }
}

function opencodeRows(): HTMLElement {
  const box = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  const install = h("button", { class: "primary", text: opencode.installed ? "Uninstall plugin" : "Install plugin", onclick: async () => {
    if (opencode.installed) await Bridge.openCodeUninstall();
    else await Bridge.openCodeInstall();
    opencode = (await Bridge.openCodeStatus()) ?? opencode;
    render();
  } });
  box.append(
    hint(opencode.installed ? "Plugin installed. Your opencode sessions show in the island." : "Coucou's plugin is not installed in opencode."),
    row("Plugin path", h("span", { class: "path", text: opencode.pluginPath || "—" })),
    row("Config dir", h("span", { class: "path", text: opencode.configDir || "—" })),
    install,
  );
  return box;
}

function sapSection(): HTMLElement {
  const fields = [
    { key: "sapb1-url", label: "Service Layer URL", placeholder: "https://host:50000", secret: false },
    { key: "sapb1-company", label: "Company DB", placeholder: "COMPANY", secret: false },
    { key: "sapb1-user", label: "User", placeholder: "manager", secret: false },
    { key: "sapb1-password", label: "Password", placeholder: "••••••••", secret: true },
  ];
  const present: Record<string, boolean> = {};
  for (const f of fields) present[f.key] = false;
  const rows = h("div", { style: "display:flex;flex-direction:column;gap:6px;flex:1 1 auto;min-width:0" });

  async function loadPresence() {
    for (const f of fields) present[f.key] = (await Bridge.secretPresent(f.key)) ?? false;
    draw();
  }

  function draw() {
    clear(rows);
    for (const f of fields) {
      const input = h("input", {
        type: f.secret ? "password" : "text",
        placeholder: present[f.key] ? "••••••••  (stored)" : f.placeholder,
        autocomplete: "off", spellcheck: "false", style: "flex:1 1 auto;min-width:0",
      }) as HTMLInputElement;
      const save = h("button", { text: "Save" });
      const dotEl = statusDot(present[f.key]);
      save.addEventListener("click", async () => {
        const v = input.value.trim();
        try {
          await Bridge.secretSet(f.key, v);
          present[f.key] = v.length > 0;
          input.value = "";
          input.placeholder = v ? "••••••••  (stored)" : f.placeholder;
          dotEl.style.background = v ? "#22c55e" : "#f4505e";
        } catch { dotEl.style.background = "#f5a524"; }
      });
      rows.append(h("div", { class: "row" }, h("label", { style: "min-width:140px", text: f.label }), input, save, dotEl));
    }
  }
  loadPresence();
  const test = h("button", { text: "Test connection", onclick: async () => {
    try {
      const probe = await Bridge.sapB1Probe();
      const ok = probe.ready;
      dialog(hint(ok ? `Connected — ${probe.entitySetCount} entity sets, ${probe.typeCount} types.` : `Not ready — missing fields in ${probe.sets.filter((s) => s.missingFields.length).length} sets.`, ok ? undefined : "#f5a524"));
    } catch (err) {
      dialog(hint(`Could not connect: ${String(err)}`, "#f4505e"));
    }
  } });
  return group("SAP Harness",
    hint("The SAP pill is status-only. Connection details live here, in the Credential Manager."),
    rows,
    h("div", { class: "row" }, h("span", { style: "flex:1" }), test),
  );
}

function dialog(content: HTMLElement) {
  const d = h("dialog", { class: "dialog" });
  const close = h("button", { text: "Close", onclick: () => d.close() });
  d.append(content, h("div", { class: "row" }, h("span", { style: "flex:1" }), close));
  document.body.append(d);
  d.showModal();
}

function loadPresence(def: typeof OTHER_INTEGRATIONS[number], present: Record<string, boolean>, host: HTMLElement) {
void (async () => {
    for (const f of def.fields) present[f.key] = (await Bridge.secretPresent(f.key)) ?? false;
    const enabled = settings.features[`integration.${def.id}`] ?? settings.activeIntegrations.includes(def.id);
    const sw = h("button", { class: enabled ? "switch on" : "switch", title: "Turn this integration on or off" });
    sw.addEventListener("click", () => {
      const on = sw.classList.contains("on");
      settings.features = { ...settings.features, [`integration.${def.id}`]: !on };
      sw.classList.toggle("on", !on);
      void save();
    });
    const rows = h("div", { style: "display:flex;flex-direction:column;gap:6px;flex:1 1 auto;min-width:0" });
    for (const f of def.fields) {
      const input = h("input", {
        type: f.secret ? "password" : "text",
        placeholder: present[f.key] ? "••••••••  (stored)" : f.placeholder,
        autocomplete: "off", spellcheck: "false", style: "flex:1 1 auto;min-width:0",
      }) as HTMLInputElement;
      const save = h("button", { text: "Save" });
      const dotEl = statusDot(present[f.key]);
      save.addEventListener("click", async () => {
        const v = input.value.trim();
        try {
          await Bridge.secretSet(f.key, v);
          present[f.key] = v.length > 0;
          input.value = "";
          input.placeholder = v ? "••••••••  (stored)" : f.placeholder;
          dotEl.style.background = v ? "#22c55e" : "#f4505e";
        } catch { dotEl.style.background = "#f5a524"; }
      });
      rows.append(h("div", { class: "row" }, h("label", { style: "min-width:104px", text: f.label }), input, save, dotEl));
    }
host.append(
      group(def.name,
        row("Active", sw),
        rows,
      ),
    );
  })();
}

// ── Appearance ────────────────────────────────────────────────────────────────

function appearancePage(): HTMLElement {
  const body = h("div", { class: "page" });
  body.append(h("h2", { text: "Appearance" }), hint("The pill row, and the flags that turn individual features on and off."));

  const pillSw = toggle(settings.pillsVisible, (v) => {
    settings.pillsVisible = v;
    void save();
    render();
  });
  body.append(group("Pills",
    row("Show pill row", pillSw),
    hint("Off shrinks the island to a single centred pill.", "#f5a524"),
  ));

  const featureDefs: { key: string; label: string }[] = [
    { key: "feature.sounds", label: "Sounds" },
    { key: "feature.motion", label: "Motion" },
    { key: "feature.badges", label: "Pill badges" },
  ];
  const pills = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  for (const def of featureDefs) {
    const on = settings.features[def.key] !== false;
    const sw = toggle(on, (v) => {
      settings.features = { ...settings.features, [def.key]: v };
      void save();
    });
    pills.append(row(def.label, sw));
  }
  body.append(group("Features", pills));

  const pillDefs = [
    { id: "integration_claude", name: "Claude Code" },
    { id: "integration_sapb1", name: "SAP Harness" },
    { id: "agent_opencode", name: "Opencode" },
    ...OTHER_INTEGRATIONS.map((x) => ({ id: x.id, name: x.name })),
  ];
  const pillList = h("div", { style: "display:flex;flex-direction:column;gap:8px" });
  for (const def of pillDefs) {
    const on = settings.pillsVisible && settings.features[`pill.${def.id}`] !== false;
    const sw = toggle(on, (v) => {
      settings.features = { ...settings.features, [`pill.${def.id}`]: v };
      void save();
    });
    pillList.append(row(def.name, sw));
  }
  body.append(group("Pills shown", pillList));

  return body;
}

// ── Advanced ──────────────────────────────────────────────────────────────────

function advancedPage(): HTMLElement {
  const body = h("div", { class: "page" });
  body.append(h("h2", { text: "Advanced" }));
  const pause = toggle(integrationsPaused, (v) => {
    integrationsPaused = v;
    void Bridge.setPaused(v);
  });
  body.append(group("Runtime",
    row("Pause integrations", pause),
    row("Version", h("span", { text: version || "—" })),
  ));
  body.append(hint("No telemetry. Network requests only go to the services you configure yourself."));
  return body;
}

// ── Router ────────────────────────────────────────────────────────────────────

function renderPage() {
  clear(pageHost);
  switch (page) {
    case "general": pageHost.append(generalPage()); break;
    case "agents": pageHost.append(agentsPage()); break;
    case "models": pageHost.append(modelsPage()); break;
    case "integrations": pageHost.append(integrationsPage()); break;
    case "appearance": pageHost.append(appearancePage()); break;
    case "advanced": pageHost.append(advancedPage()); break;
  }
}

function render() {
  const nav = root.querySelector(".settings-nav");
  if (nav) {
    for (const btn of nav.querySelectorAll("button")) {
      btn.classList.toggle("on", btn.dataset.page === page);
    }
  }
  renderPage();
}

// ── Boot ──────────────────────────────────────────────────────────────────────

async function reloadSettings() {
  const boot = await Bridge.boot();
  if (boot) settings = { ...DEFAULT_SETTINGS, ...boot.settings };
}

async function main() {
  const boot = await Bridge.boot();
  if (boot) {
    settings = { ...DEFAULT_SETTINGS, ...boot.settings };
    version = boot.version;
  }
hookStatus = (await Bridge.hooksStatus()) ?? hookStatus;
  opencode = (await Bridge.openCodeStatus()) ?? opencode;
  await loadProviderPresets();
  await loadProviderConfigs();

  const nav = h("nav", { class: "settings-nav" });
  for (const p of PAGES) {
    const btn = h("button", { class: p.id === page ? "on" : "", "data-page": p.id, text: p.label, onclick: () => {
      page = p.id;
      render();
    } });
    nav.append(btn);
  }

  clear(root);
  root.append(
    h("h1", {}, h("span", { text: "Coucou" }), h("span", { class: "version", text: version })),
    h("div", { class: "settings-body" }, nav, pageHost),
    h("div", { class: "hint", text: "No telemetry. Network requests only go to the services you configure yourself." }),
  );

  renderPage();

  void onEvent<Settings>("settings-changed", (s) => {
    settings = { ...DEFAULT_SETTINGS, ...s };
  });

  void onEvent<string>("settings-open-page", (target) => {
    if (PAGES.some((p) => p.id === target)) {
      page = target as PageId;
      render();
    }
  });
}

void main();
