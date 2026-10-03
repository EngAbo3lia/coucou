// Chat view — DOM port of PromptView / ChatBubble / TypingDotsView from
// IslandViewContent.swift.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { Bridge, type ChatContext, type SapB1Answer } from "../core/bridge";
import { Sound } from "../core/sound";
import { State, type ChatMessage } from "../core/state";
import type { ViewHost } from "./views";

let nextId = 1;

function bubble(message: ChatMessage): HTMLElement {
  if (message.role === "user") {
    return h(
      "div",
      { class: "chat-row user" },
      h("div", { class: "bubble", text: message.content }),
    );
  }
  if (message.plan) {
    // The "what I'll do" step, before the result. Subtle, so it reads as
    // planning rather than as a second answer.
    return h(
      "div",
      { class: "chat-row" },
      h("div", { class: "plan", text: message.content }),
    );
  }
  if (message.report) {
    return h(
      "div",
      { class: "chat-row" },
      reportCard(message.report),
    );
  }
  return h("div", { class: "chat-row" }, h("div", { class: "reply", text: message.content }));
}

/** A result rendered as a report: title, a bar chart, and the total. */
function reportCard(a: SapB1Answer): HTMLElement {
  const card = h("div", { class: "report" });
  const title = h("div", { class: "report-title", text: a.title });
  card.append(title);

  if (a.rows.length > 0) {
    const chart = h("div", { class: "report-chart" });
    const max = Math.max(...a.rows.map((r) => r.value), 1);
    for (const row of a.rows) {
      const pct = Math.max(2, Math.round((row.value / max) * 100));
      chart.append(
        h("div", { class: "bar-row" },
          h("span", { class: "bar-label", text: row.label }),
          h("span", { class: "bar-track" },
            h("i", { class: "bar-fill", style: `width:${pct}%` }),
          ),
          h("span", { class: "bar-value", text: money(row.value) }),
        ),
      );
    }
    card.append(chart);
  }

  if (a.total != null) {
    const total = h("div", { class: "report-total" },
      h("span", { text: "Total" }),
      h("b", { text: money(a.total) }),
    );
    if (a.partial) total.append(h("span", { class: "report-partial", text: "lower bound" }));
    card.append(total);
  }

  // The human summary line; the technical provenance is not shown to the user.
  if (a.text && a.text !== a.title) {
    card.append(h("div", { class: "report-note", text: a.text }));
  }
  return card;
}

function money(v: number): string {
  const neg = v < 0;
  const [whole, cents] = Math.abs(v).toFixed(2).split(".");
  const grouped = whole.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return `${neg ? "-" : ""}${grouped}.${cents}`;
}

function typingDots(): HTMLElement {
  return h(
    "div",
    { class: "chat-row" },
    h("div", { class: "typing" }, h("i"), h("i"), h("i")),
  );
}

/** The coloured chip showing what the question is about (a dropped file). */
function contextChip(label: string): HTMLElement {
  const chip = h("div", { class: "chip" }, h("i", { class: "chip-dot" }), h("span", { text: label }));
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

/** Same chip, plus a way out: a chat aimed at a session must be easy to stop. */
function sessionChip(label: string, onClear: () => void): HTMLElement {
  const chip = h(
    "div",
    { class: "chip" },
    h("i", { class: "chip-dot" }),
    h("span", { text: label }),
    h("b", { class: "chip-x", text: "×", onclick: onClear }),
  );
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

/** The way back to the general assistant while the chat is aimed at the ERP or at
 *  an opencode session. Pointing the chat somewhere must never be a one-way door. */
function mochiChip(onBack: () => void): HTMLElement {
  const chip = h(
    "div",
    { class: "chip alt" },
    h("span", { text: "Ask Mochi", onclick: onBack }),
  );
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

/** Attach the ERP to an open general chat, so the user can add SAP Harness after
 *  opening a normal chat. */
function attachSapChip(onAttach: () => void): HTMLElement {
  const chip = h(
    "div",
    { class: "chip" },
    h("i", { class: "chip-dot" }),
    h("span", { text: "Attach SAP", onclick: onAttach }),
  );
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

export function buildPrompt(onHeightChange: () => void): ViewHost {
  const chipRow = h("div", { class: "chip-row" });
  const log = h("div", { class: "chat-log" });
  const input = h("input", {
    type: "text",
    class: "chat-input",
    placeholder: "Ask me anything…",
    spellcheck: "false",
  }) as HTMLInputElement;
  const send = h("button", { class: "send-btn", title: "Send" }, svg(ICONS.arrowUp, 11));
  const bar = h("div", { class: "chat-bar" }, input, send);

  const el = h(
    "div",
    { class: "view" },
    h("div", { class: "card wash chat-card" }, h("div", { class: "chat-body" }, chipRow, log, bar)),
  );
  (el.querySelector(".card") as HTMLElement).style.setProperty("--wash", "rgba(99,102,241,0.5)");

  let sending = false;
  let renderedCount = -1;

  async function submit(asked?: string) {
    const query = (asked ?? input.value).trim();
    if (!query || sending) return;
    input.value = "";
    sending = true;
    Sound.play("send");

    State.chatHistory.push({ id: nextId++, role: "user", content: query });
    State.stateOverride = "thinking";
    State.chatSuggestions = [];
    State.notify();
    onHeightChange();

    const file = State.droppedFile;
    const context: ChatContext | null =
      State.chatHistory.length === 1 && file ? { kind: "file", name: file.name, path: file.path } : null;

    try {
      // A target means the message goes somewhere other than Coucou's own chat
      // provider — into an opencode session, or into the ERP. Same input box,
      // three destinations.
      const target = State.chatTarget;
      if (target?.kind === "opencode") {
        const binding = State.bindingFor("integration_opencode");
        const model = binding.provider === "opencode" && binding.model ? `${binding.provider}/${binding.model}` : "";
        const reply = await Bridge.opencodeRun(target.sessionId, target.directory, query, model);
        State.chatHistory.push({ id: nextId++, role: "assistant", content: reply });
      } else if (target?.kind === "sapb1") {
        const answer = await Bridge.sapB1Ask(query);
        State.chatSuggestions = [];
        if (answer.kind === "result") {
          // The plan bubble, then the report. Same turn, so they land together.
          if (answer.plan) {
            State.chatHistory.push({ id: nextId++, role: "assistant", content: answer.plan, plan: true });
          }
          State.chatHistory.push({ id: nextId++, role: "assistant", content: answer.text, report: answer });
        } else {
          // clarify, answer, or help: a plain message, no chips.
          State.chatHistory.push({ id: nextId++, role: "assistant", content: answer.text });
        }
      } else {
        const reply = await Bridge.chatSend(query, context);
        State.chatHistory.push({ id: nextId++, role: "assistant", content: reply.text });
      }
      State.stateOverride = null;
      Sound.play("finish");
    } catch (err) {
      State.stateOverride = null;
      State.noteMessage = String(err).replace(/^Error:\s*/, "");
      State.view = "note";
      Sound.play("error");
    } finally {
      sending = false;
      State.notify();
      onHeightChange();
      input.focus();
    }
  }

  send.addEventListener("click", () => void submit());
  input.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") {
      e.preventDefault();
      void submit();
    }
    e.stopPropagation(); // Escape closes the island, not the chat
  });

  return {
    el,
    sync() {
      const file = State.droppedFile;
      const target = State.chatTarget;
      const sapEnabled = State.tasks.some((t) => t.id === "integration_sapb1");
      const wantChip = target
        ? target.kind === "opencode"
          ? `opencode · ${target.label}`
          : target.label
        : file?.name ?? "";
      const kind = target ? "session" : file?.name ? "file" : sapEnabled ? "sap" : "";
      if (chipRow.dataset.label !== wantChip || chipRow.dataset.kind !== kind) {
        chipRow.dataset.label = wantChip;
        chipRow.dataset.kind = kind;
        clear(chipRow);
        if (target) {
          chipRow.append(sessionChip(wantChip, () => State.setChatTarget(null)));
          chipRow.append(mochiChip(() => State.setChatTarget(null)));
        } else if (wantChip) {
          chipRow.append(contextChip(wantChip));
        } else if (sapEnabled) {
          // A normal chat can still point at the ERP — SAP is never locked away.
          chipRow.append(attachSapChip(() => State.setChatTarget({ kind: "sapb1", label: "SAP Harness" })));
        }
      }

      const thinking = State.stateOverride === "thinking";
      const count = State.chatHistory.length + (thinking ? 0.5 : 0);
      if (count !== renderedCount) {
        renderedCount = count;
        clear(log);
        for (const m of State.chatHistory) log.append(bubble(m));
        if (thinking) log.append(typingDots());
        log.scrollTop = log.scrollHeight;
      }

      input.placeholder = target
        ? target.kind === "opencode"
          ? "Message this opencode session…"
          : "Ask the ERP…"
        : State.chatHistory.length === 0
          ? "Ask me anything…"
          : "Continue…";
      input.disabled = sending;
    },
    focus() {
      input.focus();
      input.select();
    },
  };
}
