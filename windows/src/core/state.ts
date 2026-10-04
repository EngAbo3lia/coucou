// App state — mirror of AppState.swift (the parts the island needs).

import { Bridge } from "./bridge";
import type { SapB1Answer } from "./bridge";
import type { BotEmoteName, BotStateName, IslandMode, IslandViewName } from "./layout";
import type { EyeShape } from "../mochi/engine";

export type AgentSource = "claudeCode" | "n8n" | "agent";
export type PillBadge = "approval" | "finished" | "error";

export interface AgentTask {
  id: string;
  name: string;
  color: string;
  state: BotStateName;
  stepIndex: number;
  steps: string[];
  source: AgentSource;
  isIntegration: boolean;
  emote?: BotEmoteName | null;
  miniEye?: EyeShape | null;
  pillBadge?: PillBadge | null;
  sessionCwd?: string | null;
  /** opencode: the session this pill is currently following, for "continue". */
  sessionId?: string | null;
  /** opencode: last known session title, shown in the sessions card. */
  sessionTitle?: string | null;
  /** opencode: the project this session runs in, shown next to the pill name. */
  projectLabel?: string | null;
}

export interface ApprovalInfo {
  requestId: string;
  sessionId: string;
  tool: string;
  command: string;
}

export interface ChatMessage {
  id: number;
  role: "user" | "assistant";
  content: string;
  /** When this assistant message is an ERP report, the structured answer. */
  report?: SapB1Answer;
  /** When this assistant message is the "what I'll do" plan bubble. */
  plan?: boolean;
}

/**
 * Where the island chat sends a message when it is not talking to the plain
 * chat provider: an opencode session, or the ERP.
 */
export type ChatTarget =
  | { kind: "opencode"; sessionId: string; directory: string; label: string }
  | { kind: "sapb1"; label: string };

/** Two targets are the same when they point at the same place. */
function sameTarget(a: ChatTarget | null, b: ChatTarget | null): boolean {
  if (!a || !b) return a === b;
  if (a.kind !== b.kind) return false;
  return a.kind === "sapb1" || a.sessionId === (b as { sessionId: string }).sessionId;
}

export type PromptContext =
  | { kind: "window"; appName: string; title: string; url?: string }
  | { kind: "file"; name: string; path?: string };

export interface ResultItem {
  label: string;
  detail: string;
  url?: string;
}

export interface SearchResult {
  title: string;
  items: ResultItem[];
  note?: string;
}

const task = (
  id: string, name: string, color: string, source: AgentSource,
): AgentTask => ({
  id, name, color, state: "idle", stepIndex: 0, steps: [], source, isIntegration: true,
});

/** AgentTask.integrationAgents — same ids, names and colours as macOS. */
export const INTEGRATION_AGENTS: AgentTask[] = [
  task("integration_claude", "VS Code", "#F5F6F8", "claudeCode"),
  task("integration_sapb1", "SAP Harness", "#0A6ED1", "n8n"),
  task("integration_resend", "Resend", "#22C55E", "n8n"),
  task("integration_n8n", "n8n", "#F29B38", "n8n"),
  task("integration_vercel", "Vercel", "#7C5CFF", "n8n"),
  task("integration_github", "GitHub", "#F4505E", "n8n"),
  task("integration_notion", "Notion", "#8C8C8C", "n8n"),
  task("integration_calcom", "Cal.com", "#C9956A", "n8n"),
  task("integration_stripe", "Stripe", "#0570DE", "n8n"),
];

/** The opencode pill. Declared while its plugin is installed, so the island
 *  shows opencode instead of the unused Claude Code hook. */
export const OPENCODE_AGENT: AgentTask = {
  id: "agent_opencode", name: "Opencode", color: "#8B5CF6",
  state: "idle", stepIndex: 0, steps: [], source: "agent", isIntegration: true,
};

export const TOGGLEABLE_INTEGRATION_IDS = [
  "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
  "integration_notion", "integration_calcom", "integration_stripe",
];

/** What an integration poller last reported. */
export interface IntegrationInfo {
  data: Record<string, unknown>;
  error: string | null;
  loaded: boolean;
  configured: boolean;
}

export interface ProviderConfig {
  /** Stable slug; also seeds the keychain key. */
  id: string;
  name: string;
  /** "anthropic" | "openai". */
  style: string;
  baseUrl: string;
  /** Keychain entry name, never the key itself. */
  keyRef: string;
  defaultModel: string;
  keyRequired: boolean;
  builtIn: boolean;
  pinnedModels: string[];
}

export interface AgentBinding {
  agentId: string;
  provider: string;
  model: string;
}

export interface Settings {
  soundEnabled: boolean;
  soundVolume: number;
  autoCloseInterval: number;
  absenceInterval: number;
  activeIntegrations: string[];
  screen: "primary" | "cursor";
  autostart: boolean;
  hooksInstalled: boolean;
  /** Saved AI backends. */
  providers: ProviderConfig[];
  /** Per-agent backend bindings. */
  agents: AgentBinding[];
  /** The backend the island chat uses. */
  provider: string;
  /** Model id for `provider`. Empty means "use the provider default". */
  model: string;
  /** Legacy: the custom endpoint. Now `providers[].baseUrl`. */
  customBaseUrl: string;
  /** Master switch for the pill row. */
  pillsVisible: boolean;
  /** Pill focused on launch. Empty means "decide at runtime". */
  defaultAgent: string;
  /** Prefixed flags: `pill.<id>`, `integration.<id>`, `feature.<name>`. */
  features: Record<string, boolean>;
}

export const DEFAULT_SETTINGS: Settings = {
  soundEnabled: true,
  soundVolume: 0.12,
  autoCloseInterval: 15,
  absenceInterval: 180,
  activeIntegrations: [
    "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
  ],
  screen: "primary",
  autostart: false,
  hooksInstalled: false,
  providers: [],
  agents: [],
  provider: "anthropic",
  model: "claude-opus-5",
  customBaseUrl: "",
  pillsVisible: true,
  defaultAgent: "",
  features: {},
};

type Listener = () => void;

class AppState {
  mode: IslandMode = "hidden";
  view: IslandViewName = "overview";

  tasks: AgentTask[] = [];
  focusId: string | null = null;

  stateOverride: BotStateName | null = null;

  /** Cursor in logical screen pixels, origin top-left (like AppState.mousePosition). */
  mouse = { x: 0, y: 0 };
  /** Cursor relative to the island's top-left corner. */
  mouseInIsland = { x: 0, y: 0 };

  isPinned = false;
  paused = false;
  /** True when Coucou's opencode plugin is installed (from Rust `boot`). */
  opencodeInstalled = false;

  uploadProgress = 0;
  uploadDuration = 2.4;
  fileDragOver = false;

  promptContext: PromptContext | null = null;
  droppedFile: { name: string; path: string } | null = null;
  noteMessage: string | null = null;
  searchResult: SearchResult | null = null;
  chatHistory: ChatMessage[] = [];
  /** When set, the island chat answers into this opencode session or the ERP. */
  chatTarget: ChatTarget | null = null;
  /** Tappable follow-ups from the last ERP answer. Empty otherwise. */
  chatSuggestions: string[] = [];
  pendingApproval: ApprovalInfo | null = null;

  integrations: Record<string, IntegrationInfo> = {};

  lastActivity = performance.now();

  settings: Settings = { ...DEFAULT_SETTINGS };

  private listeners = new Set<Listener>();

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** Marks the UI dirty; the island re-renders on the next frame. */
  notify() {
    for (const fn of this.listeners) fn();
  }

  get focusTask(): AgentTask | null {
    return this.tasks.find((t) => t.id === this.focusId) ?? this.tasks[0] ?? null;
  }

  get effectiveState(): BotStateName {
    return this.stateOverride ?? this.focusTask?.state ?? "idle";
  }

  get otherTasks(): AgentTask[] {
    return this.tasks.filter((t) => t.id !== this.focusId);
  }

  /** The chat destination a pill implies, or null when it has none of its own.
   *  Focusing the ERP and then typing must read the ERP: a question about
   *  "our orders" belongs to the ERP, never to a general chat model. */
  chatTargetFor(id: string): ChatTarget | null {
    if (id === "integration_sapb1") return { kind: "sapb1", label: "SAP Harness" };
    return null;
  }

  setFocus(id: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    this.focusId = id;
    t.pillBadge = null;
    // A chat still pointed at the pill we just left would keep answering from it.
    if (this.chatTarget?.kind === "sapb1" && id !== "integration_sapb1") {
      this.chatTarget = null;
      this.chatHistory = [];
    }
    this.notify();
  }

  /** Points the island chat at an opencode session (null clears it). */
  setChatTarget(target: ChatTarget | null) {
    const changed = !sameTarget(this.chatTarget, target);
    this.chatTarget = target;
    this.chatSuggestions = [];
    // One log, one destination: answers from the chat provider and answers from
    // an opencode session must not read as one conversation.
    if (changed) this.chatHistory = [];
    this.notify();
  }

  /** A feature flag. Absent means on, so a new flag never switches itself off. */
  feature(key: string): boolean {
    return this.settings.features[key] !== false;
  }

  /** Whether an integration is switched on. Falls back to the visible-slot list
   *  for keys a pre-flag build never stored. */
  integrationOn(id: string): boolean {
    const stored = this.settings.features[`integration.${id}`];
    if (stored !== undefined) return stored;
    return (
      id === "integration_claude" ||
      id === "integration_sapb1" ||
      this.settings.activeIntegrations.includes(id)
    );
  }

  /** Whether a pill may appear in the notch. */
  pillOn(id: string): boolean {
    return this.settings.pillsVisible && this.feature(`pill.${id}`);
  }

  /** The saved backend a provider slug maps to, or null. */
  provider(id: string): ProviderConfig | null {
    return this.settings.providers.find((p) => p.id === id) ?? null;
  }

  /** The binding for one agent, falling back to the global chat backend. */
  bindingFor(agentId: string): { provider: string; model: string } {
    const b = this.settings.agents.find((a) => a.agentId === agentId);
    if (b && this.provider(b.provider)) return { provider: b.provider, model: b.model };
    return { provider: this.settings.provider, model: this.settings.model };
  }

  /** Switches a feature flag and persists it. */
  setFeature(key: string, on: boolean) {
    this.settings.features = { ...this.settings.features, [key]: on };
    this.notify();
    void Bridge.saveSettings(this.settings);
  }

  /** Sets the master pill-row switch and persists it. */
  setPillsVisible(on: boolean) {
    this.settings.pillsVisible = on;
    this.notify();
    void Bridge.saveSettings(this.settings);
  }

  updateTask(id: string, state: BotStateName) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.state = state;
    this.notify();
  }

  appendStep(id: string, step: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.steps.push(step);
    if (t.steps.length > 20) t.steps.shift();
    t.stepIndex = t.steps.length - 1;
    this.notify();
  }

  setPillBadge(id: string, badge: PillBadge | null) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.pillBadge = badge;
    this.notify();
  }

  /** loadIntegrationTasks() — VS Code and SAP always declared, the rest opt-in
   *  (max 4). The opencode pill is declared alongside while its plugin is
   *  installed. A pill is shown when its `pill.<id>` flag is on; an integration
   *  keeps polling only while its `integration.<id>` flag is on — the two are
   *  independent, so a pill can be hidden without stopping the service. */
  loadIntegrationTasks() {
    for (const proto of INTEGRATION_AGENTS) {
      const pillOn = this.pillOn(proto.id);
      const integrationOn =
        proto.id === "integration_claude" ||
        proto.id === "integration_sapb1" ||
        this.integrationOn(proto.id);
      const shouldLoad = pillOn && integrationOn;
      const idx = this.tasks.findIndex((t) => t.id === proto.id);
      if (shouldLoad && idx < 0) this.tasks.push({ ...proto, steps: [] });
      if (!shouldLoad && idx >= 0) this.tasks.splice(idx, 1);
    }

    // opencode: declared while its plugin is installed, so its sessions stay
    // discoverable without pretending to be the Claude Code hook. Dedupe so a
    // stale task can never sit beside the declared one.
    const opencodes = this.tasks.filter((t) => t.id === OPENCODE_AGENT.id);
    if (opencodes.length > 1) {
      const keep = this.tasks.findIndex((t) => t.id === OPENCODE_AGENT.id);
      this.tasks = this.tasks.filter((t, i) => t.id !== OPENCODE_AGENT.id || i === keep);
    }
    const oidx = this.tasks.findIndex((t) => t.id === OPENCODE_AGENT.id);
    const opencodeOn = this.opencodeInstalled && this.pillOn(OPENCODE_AGENT.id);
    if (opencodeOn && oidx < 0) this.tasks.push({ ...OPENCODE_AGENT, steps: [] });
    if (!opencodeOn && oidx >= 0) this.tasks.splice(oidx, 1);

    // Order: integration_claude first, then agent_* pills (visible in slice(0,4)),
    // then other integrations in declaration order.
    const order = INTEGRATION_AGENTS.map((t) => t.id);
    this.tasks.sort((a, b) => {
      const isAgentA = a.id.startsWith("agent_");
      const isAgentB = b.id.startsWith("agent_");
      // integration_claude always first
      if (a.id === "integration_claude") return -1;
      if (b.id === "integration_claude") return 1;
      // agent_* before other integrations; preserve insertion order among themselves
      if (isAgentA && !isAgentB) return -1;
      if (isAgentB && !isAgentA) return 1;
      if (isAgentA && isAgentB) return 0;
      // both known integrations → declaration order
      return order.indexOf(a.id) - order.indexOf(b.id);
    });
    // Focus the configured default pill; otherwise SAP Harness when it is
    // present, so the ERP is the default rather than the unused Claude Code pill.
    if (!this.focusId || !this.tasks.some((t) => t.id === this.focusId)) {
      this.focusId = this.defaultFocusId();
    }
    this.syncCompact();
    this.notify();
  }

  /** The pill focused on launch: the user's choice, else SAP Harness when it is
   *  shown, else the first pill. */
  private defaultFocusId(): string {
    const chosen = this.settings.defaultAgent;
    if (chosen && this.tasks.some((t) => t.id === chosen)) return chosen;
    if (this.tasks.some((t) => t.id === "integration_sapb1")) return "integration_sapb1";
    return this.tasks[0]?.id ?? "integration_claude";
  }

  /** The island window shrinks to a single centred pill when no pill row shows.
   *  The front end knows the real visible pills; Rust just sizes the window. */
  private syncCompact() {
    const compact = this.tasks.length === 0 || !this.settings.pillsVisible;
    void Bridge.setCompact(compact);
  }

  removeTask(id: string) {
    const idx = this.tasks.findIndex((t) => t.id === id);
    if (idx < 0) return;
    this.tasks.splice(idx, 1);
    if (this.focusId === id) this.focusId = this.tasks[0]?.id ?? "integration_claude";
    this.notify();
  }

  /** Creates a dynamic agent_ pill on first event; no-ops if it already exists.
   *  Inserted right after integration_claude so it appears in the visible slice(0,4). */
  upsertExternalAgent(id: string, name: string, color: string) {
    if (this.tasks.some((t) => t.id === id)) return;
    const at = this.tasks.findIndex((t) => t.id === "integration_claude") + 1;
    this.tasks.splice(at, 0, {
      id, name, color,
      state: "idle", stepIndex: 0, steps: [],
      source: "agent", isIntegration: false,
    });
    if (!this.focusId) this.focusId = id;
    this.notify();
  }

  toggleIntegration(id: string) {
    if (id === "integration_claude") return;
    const active = this.settings.activeIntegrations;
    const wasActive = active.includes(id);
    if (wasActive) {
      this.settings.activeIntegrations = active.filter((x) => x !== id);
      if (this.focusId === id) this.focusId = "integration_claude";
    } else {
      if (active.length >= 4) return;
      this.settings.activeIntegrations = [...active, id];
    }
    this.settings.features = { ...this.settings.features, [`integration.${id}`]: !wasActive };
    this.loadIntegrationTasks();
    void Bridge.saveSettings(this.settings);
  }

  defaultView(): IslandViewName {
    return this.tasks.length === 0 ? "empty" : "overview";
  }
}

export const State = new AppState();
