// Integration cards shown in the overview's left card — DOM ports of
// IntegrationCardView and friends from IslandViewContent.swift.
//
// Cal.com is the one simplification: macOS shows a three-level calendar
// (month → day → booking); here it is the list of upcoming bookings.

import { h, svg, clear, dot } from "./dom";
import { ICONS } from "./icons";
import { State, type AgentTask, type IntegrationInfo } from "../core/state";
import { Bridge, type OpencodeSession, type SapB1Probe } from "../core/bridge";

/** Same shape as the Swift `timeAgo` computed properties. */
export function timeAgo(value: unknown): string {
  const date = typeof value === "number" ? new Date(value) : new Date(String(value));
  const diff = (Date.now() - date.getTime()) / 1000;
  if (!Number.isFinite(diff)) return "";
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}

function header(color: string, name: string, kind: string, extra?: Node): HTMLElement {
  const row = h("div", { class: "int-head" }, dot(color, 7), h("b", { text: name }), h("span", { text: kind }));
  if (extra) row.append(extra);
  return row;
}

/** Highlighted first row + plain rows, the layout every list card shares. */
function listRow(accent: string, first: boolean, ...children: Node[]): HTMLElement {
  const row = h("div", { class: first ? "int-row first" : "int-row" }, dot(accent, 5), ...children);
  if (first) row.style.background = `${accent}14`;
  return row;
}

function get(id: string): Record<string, unknown> {
  return (State.integrations[id]?.data ?? {}) as Record<string, unknown>;
}

function arr(id: string, key: string): Record<string, unknown>[] {
  const v = get(id)[key];
  return Array.isArray(v) ? (v as Record<string, unknown>[]) : [];
}

// ── Not configured / idle ─────────────────────────────────────────────────────

const OPEN_URLS: Record<string, string> = {
  integration_resend: "https://resend.com/emails",
  integration_vercel: "https://vercel.com/dashboard",
  integration_github: "https://github.com",
  integration_stripe: "https://dashboard.stripe.com/payments",
  integration_notion: "https://notion.so",
  integration_calcom: "https://app.cal.com/bookings",
};

function idleCard(task: AgentTask, openSettings: () => void): HTMLElement {
  const info = State.integrations[task.id];
  const configured = info?.configured ?? false;
  const error = info?.error ?? null;
  // The Claude Code pill is about hooks, the opencode pill about its plugin —
  // the macOS wording would be misleading here.
  const missing = task.id === "integration_claude" ? "Hooks not installed"
    : task.id === "agent_opencode" ? "Plugin not installed"
    : "Key not configured";
  const ready = task.id === "agent_opencode" ? "Watching for sessions" : "Connected · loading…";
  const label = error ?? (configured ? ready : missing);
  const statusColor = error || !configured ? "#F4505E" : "#22C55E";

  const actions = h("div", { class: "int-actions" });
  if (task.id === "integration_claude") {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}b3`,
        text: "Open Visual Studio Code",
        onclick: () => void Bridge.openInVSCode(task.sessionCwd ?? null),
      }),
    );
  } else if (task.id === "integration_n8n") {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: "Open n8n",
        onclick: () => void Bridge.openN8n(),
      }),
    );
  } else if (OPEN_URLS[task.id]) {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: `Open ${task.name}`,
        onclick: () => void Bridge.openUrl(OPEN_URLS[task.id]),
      }),
    );
  }
  if (task.id === "agent_opencode") {
    // No poller to refresh — the useful action is managing the plugin.
    actions.append(
      h("button", { class: "link-btn", style: "color:#8e939c", text: "Settings…", onclick: openSettings }),
    );
  } else if (configured) {
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: "Refresh",
        onclick: () => void Bridge.refreshIntegration(task.id),
      }),
    );
  } else {
    actions.append(
      h("button", { class: "link-btn", style: "color:#8e939c", text: "Settings…", onclick: openSettings }),
    );
  }

  const subtitle = task.id === "agent_opencode" ? "Agent" : "Integration";
  return h(
    "div",
    { class: "int-card" },
    header(task.color, task.id === "integration_claude" ? "VS Code" : task.name, subtitle),
    h("div", { class: "int-status" }, dot(statusColor, 5), h("span", { text: label })),
    actions,
  );
}

// ── opencode sessions ─────────────────────────────────────────────────────────

/** Guards against two renders both kicking off a session fetch. */
let opencodeFetching = false;
/** Which row the Continue buttons act on. Null means "the newest session". */
let selectedOpencodeSession: string | null = null;

function lastComponent(p: string): string {
  const cleaned = p.replace(/[\\/]+$/, "");
  const i = Math.max(cleaned.lastIndexOf("\\"), cleaned.lastIndexOf("/"));
  return i >= 0 ? cleaned.slice(i + 1) : cleaned;
}

function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 8)}…` : id;
}

/** Loads recent sessions once, caching them the way a poller would. */
function loadOpencodeSessions() {
  if (opencodeFetching) return;
  opencodeFetching = true;
  void Bridge.opencodeSessions(10)
    .then((sessions) => {
      State.integrations.agent_opencode = {
        data: { sessions }, error: null, loaded: true, configured: true,
      };
      State.notify();
    })
    .catch((err) => {
      State.integrations.agent_opencode = {
        data: {}, error: String(err), loaded: false, configured: true,
      };
      State.notify();
    })
    .finally(() => {
      opencodeFetching = false;
    });
}

function skeletonRow(): HTMLElement {
  const bar = (width: string) =>
    h("span", { style: `width:${width};height:8px;border-radius:4px;background:#ffffff1a` });
  return h("div", { class: "int-row" }, bar("45%"), bar("18%"));
}

function sessionRow(
  session: OpencodeSession,
  selected: boolean,
  onSelect: () => void,
): HTMLElement {
  const label = session.title?.trim() || shortId(session.id);
  const row = listRow("#8B5CF6", selected,
    h("span", { class: "int-name", text: label, title: label }),
    h("span", { class: "int-sub", text: session.directory ? lastComponent(session.directory) : "" }),
    h("span", { class: "int-ago", text: timeAgo(session.updated) }),
  );
  if (session.live) row.prepend(dot("#22C55E", 5));
  row.style.cursor = "pointer";
  row.onclick = onSelect;
  return row;
}

/** The opencode pill's card: live + recent sessions. Pick a row, then Continue. */
export function opencodeCard(task: AgentTask, hooks: IntegrationCardHooks): HTMLElement {
  const info = State.integrations["agent_opencode"];
  const sessions = (info?.data as { sessions?: OpencodeSession[] } | undefined)?.sessions ?? null;
  const body = h("div", { class: "int-body" });

  const retry = () => {
    delete State.integrations.agent_opencode;
    State.notify();
  };

  let selected: OpencodeSession | null = null;
  if (info?.error) {
    body.append(
      h("div", { class: "int-status" }, dot("#F4505E", 5), h("span", { text: "Could not read sessions" })),
      h("button", { class: "link-btn", style: "color:#8e939c", text: "Retry", onclick: retry }),
    );
  } else if (!sessions) {
    loadOpencodeSessions();
    body.append(skeletonRow(), skeletonRow(), skeletonRow());
  } else if (sessions.length === 0) {
    body.append(
      h("div", { class: "int-status" }, dot("#8e939c", 5), h("span", { text: "No opencode session yet" })),
    );
  } else {
    // A row the island no longer lists must not stay the target of Continue.
    if (selectedOpencodeSession && !sessions.some((s) => s.id === selectedOpencodeSession)) {
      selectedOpencodeSession = null;
    }
    selected = sessions.find((s) => s.id === selectedOpencodeSession) ?? sessions[0];
    sessions.forEach((s) =>
      body.append(
        sessionRow(s, s.id === selected?.id, () => {
          selectedOpencodeSession = s.id;
          State.notify();
        }),
      ),
    );
  }

  const actions = h("div", { class: "int-actions" });
  if (selected) {
    const target = {
      id: selected.id,
      directory: selected.directory,
      title: selected.title?.trim() || lastComponent(selected.directory) || shortId(selected.id),
    };
    actions.append(
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: "Continue: Chat",
        onclick: () => hooks.continueInChat(target),
      }),
      h("button", {
        class: "link-btn",
        style: `color:${task.color}d9`,
        text: "Continue: Opencode",
        onclick: () => hooks.continueInOpencode(target),
      }),
      h("button", { class: "link-btn", style: "color:#8e939c", text: "Refresh", onclick: retry }),
    );
  }
  actions.append(
    h("button", { class: "link-btn", style: "color:#8e939c", text: "Settings…", onclick: hooks.openSettings }),
  );

  return h(
    "div",
    { class: "int-card" },
    header(task.color, task.name, "Agent"),
    body,
    actions,
  );
}

// ── Vercel ────────────────────────────────────────────────────────────────────

function vercelCard(onDetail: () => void): HTMLElement {
  const deployments = arr("integration_vercel", "deployments");
  const rows = h("div", { class: "int-rows" });
  deployments.slice(0, 3).forEach((d, i) => {
    const accent = d.state === "READY" ? "#22C55E" : "#F4505E";
    const name = h("span", { class: "int-name", text: String(d.projectName ?? "") });
    const ago = h("span", { class: "int-ago", text: timeAgo(d.createdAt) });
    if (i === 0) {
      const more = h(
        "button",
        { class: "int-more", title: "Details", onclick: onDetail },
        svg(ICONS.ellipsis, 8),
      );
      rows.append(listRow(accent, true, name, ago, more));
    } else {
      rows.append(listRow(accent, false, name, ago));
    }
  });
  return h("div", { class: "int-card" }, header("#7C5CFF", "Vercel", "Deployments"), rows);
}

function vercelDetail(onBack: () => void): HTMLElement {
  const d = arr("integration_vercel", "deployments")[0] ?? {};
  const success = d.state === "READY";
  const accent = success ? "#22C55E" : "#F4505E";
  const status = success ? "Ready" : d.state === "CANCELED" ? "Canceled" : "Error";
  const body = h("div", { class: "int-detail-body" });
  if (d.commitMessage) body.append(h("div", { class: "int-commit", text: String(d.commitMessage) }));
  const meta = h("div", { class: "int-meta" });
  if (d.branch) meta.append(h("span", { text: String(d.branch) }));
  meta.append(h("span", { text: `${timeAgo(d.createdAt)} ago` }));
  body.append(meta);
  if (d.url) {
    body.append(
      h("button", {
        class: "int-link",
        text: String(d.url),
        onclick: () => void Bridge.openUrl(`https://${d.url}`),
      }),
    );
  }
  return h(
    "div",
    { class: "int-card detail" },
    h(
      "div",
      { class: "int-detail-head" },
      h("button", { class: "int-back", onclick: onBack }, svg(ICONS.chevronLeft, 10, { stroke: 2.4 })),
      dot(accent, 6),
      h("b", { text: String(d.projectName ?? "Deployment") }),
      h("span", { class: "int-badge", style: `color:${accent};background:${accent}24`, text: status }),
    ),
    body,
  );
}

// ── Resend ────────────────────────────────────────────────────────────────────

function resendCard(): HTMLElement {
  const emails = arr("integration_resend", "emails");
  const total = get("integration_resend").total;
  const extra =
    total != null
      ? h("span", { class: "int-total" }, h("i", { class: "pulse" }), h("span", { text: String(total) }))
      : undefined;
  const rows = h("div", { class: "int-rows" });
  emails.slice(0, 3).forEach((e, i) => {
    const delivered = e.lastEvent === "delivered";
    const accent = delivered ? "#22C55E" : "#F4505E";
    const to = Array.isArray(e.to) ? String(e.to[0] ?? "?") : "?";
    const short = to.split("@")[0];
    const cells: Node[] = [
      h("span", { class: "int-name", text: short }),
      h("span", { class: "int-ago", text: timeAgo(e.createdAt) }),
    ];
    if (i === 0 && e.subject) cells.push(h("span", { class: "int-sub", text: String(e.subject) }));
    rows.append(listRow(accent, i === 0, ...cells));
  });
  return h("div", { class: "int-card" }, header("#22C55E", "Resend", "Emails", extra), rows);
}

// ── GitHub ────────────────────────────────────────────────────────────────────

function statRow(icon: string, color: string, label: string, value: string): HTMLElement {
  return h(
    "div",
    { class: "int-stat" },
    h("i", { class: "int-stat-icon", style: `color:${color}` }, svg(icon, 10)),
    h("span", { class: "int-stat-label", text: label }),
    h("span", { class: "int-stat-value", text: value }),
  );
}

function githubCard(): HTMLElement {
  const d = get("integration_github");
  const stars = Number(d.totalStars ?? 0);
  const repos = Number(d.totalRepos ?? 0);
  const fmt = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));
  return h(
    "div",
    { class: "int-card" },
    header("#F4505E", "GitHub", "Overview"),
    h(
      "div",
      { class: "int-stats" },
      statRow(ICONS.star, "#F5A524", "Total stars", fmt(stars)),
      statRow(ICONS.stack, "#6B7079", "Repositories", String(repos)),
    ),
  );
}

// ── Stripe ────────────────────────────────────────────────────────────────────

function stripeCard(): HTMLElement {
  const d = get("integration_stripe");
  const balance = (Number(d.balance ?? 0) / 100).toFixed(2);
  const currency = String(d.currency ?? "eur").toUpperCase();
  const rows = h("div", { class: "int-rows tight" });
  for (const p of arr("integration_stripe", "payments")) {
    const success = p.status === "succeeded";
    const accent = success ? "#22C55E" : "#F4505E";
    rows.append(
      h(
        "div",
        { class: "int-row" },
        dot(accent, 5),
        h("span", { class: "int-name", text: String(p.description ?? "Payment") }),
        h("span", {
          class: "int-amount",
          style: "color:#22c55e",
          text: `+${(Number(p.amount ?? 0) / 100).toFixed(2)}`,
        }),
        h("span", { class: "int-ago", text: timeAgo(p.createdAt) }),
      ),
    );
  }
  return h(
    "div",
    { class: "int-card" },
    header("#0570DE", "Stripe", "Payments"),
    h("div", { class: "int-balance" }, h("span", { text: balance }), h("i", { text: currency })),
    rows,
  );
}

// ── Notion ────────────────────────────────────────────────────────────────────

function notionCard(): HTMLElement {
  const rows = h("div", { class: "int-rows tight" });
  for (const p of arr("integration_notion", "pages").slice(0, 3)) {
    rows.append(
      h(
        "button",
        {
          class: "int-page",
          onclick: () => {
            if (typeof p.url === "string") void Bridge.openUrl(p.url);
          },
        },
        p.emoji
          ? h("span", { class: "int-emoji", text: String(p.emoji) })
          : h("i", { class: "int-emoji" }, svg(ICONS.doc, 9)),
        h("span", { class: "int-name", text: String(p.title ?? "Untitled") }),
        h("span", { class: "int-ago", text: timeAgo(p.lastEditedAt) }),
      ),
    );
  }
  return h("div", { class: "int-card" }, header("#E8E8E8", "Notion", "Recent"), rows);
}

// ── Cal.com ───────────────────────────────────────────────────────────────────

function calcomCard(): HTMLElement {
  const bookings = arr("integration_calcom", "bookings")
    .slice()
    .sort((a, b) => new Date(String(a.start)).getTime() - new Date(String(b.start)).getTime());
  const rows = h("div", { class: "int-rows tight" });
  if (bookings.length === 0) {
    rows.append(h("div", { class: "int-empty", text: "No calls scheduled" }));
  }
  for (const b of bookings.slice(0, 3)) {
    const when = new Date(String(b.start));
    const day = when.toLocaleDateString(undefined, { day: "2-digit", month: "2-digit" });
    const time = when.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
    rows.append(
      h(
        "div",
        { class: "int-row" },
        dot("#C9956A", 4),
        h("span", { class: "int-time", text: `${day} ${time}` }),
        h("span", { class: "int-name", text: String(b.title ?? "Meeting") }),
      ),
    );
  }
  return h("div", { class: "int-card" }, header("#C9956A", "Cal.com", "Schedule"), rows);
}

// ── n8n ───────────────────────────────────────────────────────────────────────

function n8nCard(task: AgentTask, onDetail: () => void, openSettings: () => void): HTMLElement {
  const hasActivity = task.steps.length > 0 && (task.state === "finished" || task.state === "error");
  if (!hasActivity) return idleCard(task, openSettings);
  const success = task.state === "finished";
  const accent = success ? "#22C55E" : "#F4505E";
  return h(
    "div",
    { class: "int-card" },
    header("#F29B38", "n8n", "Workflow"),
    h(
      "div",
      { class: "int-actions" },
      h(
        "button",
        {
          class: "int-pill",
          style: `background:${accent}1a;border-color:${accent}38`,
          onclick: onDetail,
        },
        dot(accent, 5),
        h("span", { class: "int-name", text: task.steps[0] ?? "Workflow" }),
        svg(ICONS.ellipsis, 8),
      ),
    ),
  );
}

function n8nDetail(task: AgentTask, onBack: () => void): HTMLElement {
  const success = task.state === "finished";
  const accent = success ? "#22C55E" : "#F4505E";
  const detail = task.steps[1];
  return h(
    "div",
    { class: "int-card detail" },
    h(
      "div",
      { class: "int-detail-head" },
      h("button", { class: "int-back", onclick: onBack }, svg(ICONS.chevronLeft, 10, { stroke: 2.4 })),
      dot(accent, 6),
      h("b", { text: task.steps[0] ?? "Workflow" }),
      h("span", {
        class: "int-badge",
        style: `color:${accent};background:${accent}24`,
        text: success ? "Success" : "Failed",
      }),
    ),
    detail
      ? h("pre", { class: "int-detail-text", text: detail })
      : h("div", {
          class: "int-status",
          text: success ? "Completed successfully." : "No error details available.",
        }),
  );
}

// ── SAP Harness ───────────────────────────────────────────────────────────────
//
// The one card that configures itself: the ERP is a company database, not a
// public service, so its credentials live here in the pill instead of a
// Settings row. Passwords go straight to the Credential Manager and are never
// read back — only presence is ever known.

const SAP_FIELDS: { key: string; label: string; placeholder: string; secret: boolean }[] = [
  { key: "sapb1-url", label: "Service Layer URL", placeholder: "https://host:50000", secret: false },
  { key: "sapb1-company", label: "Company DB", placeholder: "TESTING01", secret: false },
  { key: "sapb1-user", label: "User", placeholder: "manager", secret: false },
  { key: "sapb1-password", label: "Password", placeholder: "…", secret: true },
];

/** Which SAP keys exist, fetched once: values are never readable. */
let sapKeys: Record<string, boolean> | null = null;
let sapKeysLoading = false;
/** Guards against two clicks both starting a login. */
let sapProbing = false;
/** Guards the "connect, then open the chat" path in the Ask the ERP button. */
let sapAsking = false;

async function loadSapKeys() {
  if (sapKeys || sapKeysLoading) return;
  sapKeysLoading = true;
  try {
    const found = await Promise.all(
      SAP_FIELDS.map(async (f) => [f.key, (await Bridge.secretPresent(f.key)) === true] as const),
    );
    sapKeys = Object.fromEntries(found);
  } finally {
    sapKeysLoading = false;
    State.notify();
  }
}

/** Writes the card's slice of integration state and repaints. */
function setSapState(id: string, patch: Partial<IntegrationInfo>) {
  const info = State.integrations[id] ?? { data: {}, error: null, loaded: false, configured: false };
  State.integrations[id] = { ...info, ...patch };
  State.notify();
}

export function sapB1Card(task: AgentTask, hooks: IntegrationCardHooks): HTMLElement {
  const info = State.integrations[task.id];
  const probe = (info?.data as { probe?: SapB1Probe } | undefined)?.probe ?? null;
  if (!sapKeys) void loadSapKeys();

  const stored = (key: string) => sapKeys?.[key] === true;
  const missing = sapKeys ? SAP_FIELDS.filter((f) => !stored(f.key)) : [];
  const complete = sapKeys !== null && missing.length === 0;

  const label = info?.error
    ?? (probe
      ? probe.ready
        ? `Connected · ${probe.entitySetCount} entity sets · all report fields present`
        : `Connected · missing ${probe.sets.filter((s) => !s.present || s.missingFields.length > 0).map((s) => s.entitySet).join(", ")}`
      : sapProbing
        ? "Connecting…"
        : complete
          ? "Credentials stored · not tested yet"
          : sapKeys
            ? `Missing: ${missing.map((f) => f.label).join(", ")}`
            : "Checking credentials…");
  const statusColor = info?.error ? "#F4505E"
    : probe?.ready ? "#22C55E"
    : complete ? "#f5a524"
    : "#F4505E";

  const actions = h("div", { class: "int-actions" });
  const test = h("button", { class: "link-btn", style: `color:${task.color}d9`, text: "Test connection" });
  const chat = h("button", {
    class: "link-btn",
    style: `color:${probe?.ready ? `${task.color}d9` : "#8e939c"}`,
    text: "Ask the ERP…",
  });
  const chatHint = h("span", { class: "hint" });
  if (!probe?.ready && sapKeys) {
    chatHint.textContent = complete
      ? "Not tested yet — this will connect first."
      : `Add ${missing.map((f) => f.label).join(", ")} first.`;
  }

  const configure = h("button", {
    class: "link-btn",
    text: "Configure…",
    onclick: () => void Bridge.openSettingsWindow("integrations"),
  });

  // Never a dead button: if the ERP has not been proven reachable yet, connect
  // now and only open the chat once the server actually answered.
  chat.addEventListener("click", () => void (async () => {
    if (sapProbing || sapAsking) return;
    if (probe?.ready) { hooks.chatWithErp(); return; }
    if (!complete) { chatHint.textContent = `Add ${missing.map((f) => f.label).join(", ")} first.`; return; }
    sapAsking = true;
    chat.textContent = "Connecting…";
    setSapState(task.id, { error: null });
    try {
      const result = await Bridge.sapB1Probe();
      setSapState(task.id, { data: { probe: result }, error: null, loaded: true, configured: true });
      if (result.ready) hooks.chatWithErp();
      else chatHint.textContent = "The server answered but is not ready — see the status above.";
    } catch (err) {
      const message = String(err).replace(/^Error:\s*/, "");
      setSapState(task.id, { error: message });
      chatHint.textContent = message;
    } finally {
      sapAsking = false;
      chat.textContent = "Ask the ERP…";
      State.notify();
    }
  })());
  actions.append(test, chat, configure, chatHint);

  test.addEventListener("click", () => void (async () => {
    if (sapProbing) return;
    sapProbing = true;
    setSapState(task.id, { error: null });
    try {
      const result = await Bridge.sapB1Probe();
      setSapState(task.id, { data: { probe: result }, error: null, loaded: true, configured: true });
    } catch (err) {
      setSapState(task.id, { error: String(err).replace(/^Error:\s*/, "") });
    } finally {
      sapProbing = false;
      State.notify();
    }
  })());

  return h(
    "div",
    { class: "int-card" },
    header(task.color, task.name, "ERP"),
    h("div", { class: "int-status" }, dot(statusColor, 5), h("span", { text: label })),
    actions,
  );
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

export interface IntegrationCardHooks {
  detailOpen: boolean;
  openDetail(): void;
  closeDetail(): void;
  openSettings(): void;
  /** Continue a session in the island chat instead of a terminal. */
  continueInChat(session: { id: string; directory: string; title: string }): void;
  /** Continue a session in opencode's own window. */
  continueInOpencode(session: { id: string; directory: string; title: string }): void;
  /** Point the island chat at the ERP. */
  chatWithErp(): void;
}

/** True when this integration has data worth showing instead of the idle card. */
export function hasIntegrationData(id: string): boolean {
  const info = State.integrations[id];
  if (!info || info.error) return false;
  switch (id) {
    case "integration_vercel":
      return arr(id, "deployments").length > 0;
    case "integration_resend":
      return arr(id, "emails").length > 0;
    case "integration_github":
      return get(id).totalRepos != null;
    case "integration_stripe":
      return info.loaded;
    case "integration_notion":
      return arr(id, "pages").length > 0;
    case "integration_calcom":
      return info.loaded;
    default:
      return false;
  }
}

export function renderIntegrationCard(task: AgentTask, hooks: IntegrationCardHooks): HTMLElement {
  if (task.id === "agent_opencode") {
    return opencodeCard(task, hooks);
  }
  if (task.id === "integration_sapb1") {
    return sapB1Card(task, hooks);
  }
  if (task.id === "integration_n8n") {
    const hasActivity = task.steps.length > 0 && (task.state === "finished" || task.state === "error");
    return hooks.detailOpen && hasActivity
      ? n8nDetail(task, hooks.closeDetail)
      : n8nCard(task, hooks.openDetail, hooks.openSettings);
  }
  if (task.id === "integration_vercel" && hasIntegrationData(task.id)) {
    return hooks.detailOpen ? vercelDetail(hooks.closeDetail) : vercelCard(hooks.openDetail);
  }
  if (!hasIntegrationData(task.id)) return idleCard(task, hooks.openSettings);

  switch (task.id) {
    case "integration_resend":
      return resendCard();
    case "integration_github":
      return githubCard();
    case "integration_stripe":
      return stripeCard();
    case "integration_notion":
      return notionCard();
    case "integration_calcom":
      return calcomCard();
    default:
      return idleCard(task, hooks.openSettings);
  }
}

export { clear };
