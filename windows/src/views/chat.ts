// Chat view — DOM port of PromptView / ChatBubble / TypingDotsView from
// IslandViewContent.swift.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { Bridge, type ChatContext, type SapB1Answer, type SapB1DocumentSpec, type SapB1EntitySpec, type SapB1LineSpec } from "../core/bridge";
import { Sound } from "../core/sound";
import { State, type ChatMessage } from "../core/state";
import type { ViewHost } from "./views";

let nextId = 1;

/** TEMP (test builds only): flatten a message onto one log line so the whole
 *  conversation can be read back from coucou.log. Remove before release. */
function logText(text: string): string {
  return text.replace(/\s+/g, " ").trim().slice(0, 2000);
}

function bubble(message: ChatMessage, onPick?: (value: string) => void): HTMLElement {
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
      reportCard(message.report, onPick),
    );
  }
  return h("div", { class: "chat-row" }, h("div", { class: "reply", text: message.content }));
}

/** A result rendered as a report: title, a bar chart, and the total. */
function reportCard(a: SapB1Answer, onPick?: (value: string) => void): HTMLElement {
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

  // The human note, only when there is no chart to read. A report with rows
  // already shows the figures as bars, so a second text block would duplicate.
  if (a.text && a.text !== a.title && a.rows.length === 0) {
    card.append(h("div", { class: "report-note", text: a.text }));
  }

  // Clickable choices: tapping one sends its value as the next message, so the
  // user picks an item or customer instead of typing a code.
  if (a.options.length > 0 && onPick) {
    const picks = h("div", { class: "picks" });
    for (const o of a.options) {
      picks.append(h("button", { class: "pick", text: o.label, onclick: () => onPick(o.value) }));
    }
    card.append(picks);
  }
  return card;
}

function money(v: number): string {
  const neg = v < 0;
  const [whole, cents] = Math.abs(v).toFixed(2).split(".");
  const grouped = whole.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return `${neg ? "-" : ""}${grouped}.${cents}`;
}

/** What the confirm dialog resolved to: a document to post, or an entity form
 *  whose field values are ready, or null when the user cancelled. */
type ConfirmResult =
  | { kind: "document"; spec: SapB1DocumentSpec }
  | { kind: "entity"; set: string; values: Record<string, string>; lines: SapB1LineSpec[] };

/** Asks for an explicit confirm before anything is posted. For a document the
 *  user sets each line's quantity and price; for an entity (item, partner,
 *  goods receipt) every field the entity needs is shown as an input. No silent
 *  create. */
function confirmDocument(a: SapB1Answer): Promise<ConfirmResult | null> {
  return new Promise((resolve) => {
    const payload = a.payload;
    const d = h("dialog", { class: "confirm" });
    d.append(h("div", { class: "confirm-title", text: a.title }));

    const editors: { qty: HTMLInputElement; price: HTMLInputElement }[] = [];
    const lineEditors = (lines: SapB1LineSpec[]) => {
      const list = h("div", { class: "confirm-lines" });
      for (const line of lines) {
        const qty = h("input", {
          class: "confirm-input", type: "number", min: "0", step: "1",
          value: String(line.quantity),
        }) as HTMLInputElement;
        const price = h("input", {
          class: "confirm-input", type: "number", min: "0", step: "0.01",
          value: line.price != null ? String(line.price) : "",
        }) as HTMLInputElement;
        list.append(
          h("div", { class: "confirm-line" },
            h("span", { class: "confirm-item", text: line.itemCode }),
            qty, price,
          ),
        );
        editors.push({ qty, price });
      }
      return list;
    };

    if (payload && "fields" in payload) {
      // Entity form: one labelled input per field the entity declares.
      const spec = payload as SapB1EntitySpec;
      const inputs = new Map<string, HTMLInputElement | HTMLSelectElement>();
      const form = h("div", { class: "confirm-form" });
      for (const field of spec.fields) {
        let control: HTMLInputElement | HTMLSelectElement;
        if (field.kind === "choice") {
          const select = h("select", { class: "confirm-input" }) as HTMLSelectElement;
          for (const option of field.options) {
            select.append(h("option", { value: option, text: option }));
          }
          select.value = field.value;
          control = select;
        } else {
          const type = field.kind === "number" ? "number" : field.kind === "date" ? "date" : "text";
          control = h("input", { class: "confirm-input", type, value: field.value }) as HTMLInputElement;
        }
        form.append(
          h("label", { class: "confirm-field" },
            h("span", { class: "confirm-label", text: field.required ? `${field.label} *` : field.label }),
            control,
          ),
        );
        inputs.set(field.key, control);
      }
      d.append(form);
      if (spec.lines.length) {
        d.append(
          h("div", { class: "confirm-cols" },
            h("span", { text: "Item" }), h("span", { text: "Qty" }), h("span", { text: "Price" }),
          ),
          lineEditors(spec.lines),
        );
      }
      const ok = h("button", {
        class: "confirm-ok", text: "Create",
        onclick: () => {
          d.close();
          const values: Record<string, string> = {};
          for (const [key, input] of inputs) { values[key] = input.value; }
          const lines = spec.lines.map((line, i) => ({
            itemCode: line.itemCode,
            quantity: Number(editors[i].qty.value) || 0,
            price: editors[i].price.value.trim() === "" ? null : Number(editors[i].price.value),
          }));
          resolve({ kind: "entity", set: spec.set, values, lines });
        },
      });
      d.append(h("div", { class: "confirm-actions" }, cancelButton(d, resolve), ok));
    } else if (payload) {
      const spec = payload as SapB1DocumentSpec;
      d.append(
        h("div", { class: "confirm-cols" },
          h("span", { text: "Item" }), h("span", { text: "Qty" }), h("span", { text: "Price" }),
        ),
        lineEditors(spec.lines),
        h("div", { class: "confirm-hint", text: `${spec.cardCode}${spec.docDate ? ` · ${spec.docDate}` : ""}` }),
      );
      if (spec.baseEntry) {
        // On a copy the server reads price, tax and currency from the source
        // document, so saying so stops a user hunting for a wrong total.
        d.append(h("div", {
          class: "confirm-hint",
          text: `Copied from document ${spec.baseEntry}. Price and tax come from that document.`,
        }));
      }
      const ok = h("button", {
        class: "confirm-ok", text: "Create",
        onclick: () => {
          d.close();
          const lines = spec.lines.map((line, i) => ({
            itemCode: line.itemCode,
            quantity: Number(editors[i].qty.value) || 0,
            price: editors[i].price.value.trim() === "" ? null : Number(editors[i].price.value),
            // A copy keeps its source line even after the user edits the figures.
            baseLine: line.baseLine ?? null,
          }));
          resolve({ kind: "document", spec: { ...spec, lines } });
        },
      });
      d.append(h("div", { class: "confirm-actions" }, cancelButton(d, resolve), ok));
    } else {
      d.append(h("div", { class: "confirm-body", text: a.text }));
      d.append(h("div", { class: "confirm-actions" }, cancelButton(d, resolve)));
    }

    document.body.append(d);
    d.showModal();
  });
}

/** The Cancel button, shared by both forms. */
function cancelButton(
  d: HTMLDialogElement,
  resolve: (v: ConfirmResult | null) => void,
): HTMLButtonElement {
  return h("button", {
    class: "confirm-cancel", text: "Cancel",
    onclick: () => { d.close(); resolve(null); },
  });
}

/** A short confirmation after the server accepted the document. */
function createdMessage(result: unknown): string {
  const r = (result ?? {}) as Record<string, unknown>;
  const num = r.DocNum ?? r.DocEntry;
  return num != null ? `Created — document ${num}.` : "Created.";
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
    const target = State.chatTarget;
    // TEMP (test builds): destination, timing and the full question and answer.
    // Remove before release — an ERP reply can carry customer data.
    const dest = target?.kind ?? (file ? "file" : "provider");
    const started = Date.now();
    void Bridge.log(`chat > ${dest} ${logText(query)}`);

    try {
      // A target means the message goes somewhere other than Coucou's own chat
      // provider — into an opencode session, or into the ERP. Same input box,
      // three destinations.
      if (target?.kind === "opencode") {
        const binding = State.bindingFor("integration_opencode");
        const model = binding.provider === "opencode" && binding.model ? `${binding.provider}/${binding.model}` : "";
        const reply = await Bridge.opencodeRun(target.sessionId, target.directory, query, model);
        State.chatHistory.push({ id: nextId++, role: "assistant", content: reply });
      } else if (target?.kind === "sapb1") {
        // Send the conversation so far so the planner stays aware and does not
        // re-ask a question it already asked. Exclude the message we just added.
        const history = State.chatHistory
          .slice(0, -1)
          .slice(-20)
          .map((m) => ({ role: m.role, content: m.content }));
        const answer = await Bridge.sapB1Ask(query, history);
        State.chatSuggestions = [];
        if (answer.kind === "confirm") {
          // Show the preview, then ask for an explicit click before posting.
          State.chatHistory.push({ id: nextId++, role: "assistant", content: answer.text });
          const spec = await confirmDocument(answer);
          if (spec) {
            const result = spec.kind === "entity"
              ? await Bridge.sapB1WriteEntity(spec.set, spec.values, spec.lines)
              : await Bridge.sapB1CreateDocument(spec.spec);
            State.chatHistory.push({ id: nextId++, role: "assistant", content: createdMessage(result) });
          }
        } else if (answer.kind === "result") {
          // The plan bubble, then the report. Same turn, so they land together.
          if (answer.plan) {
            State.chatHistory.push({ id: nextId++, role: "assistant", content: answer.plan, plan: true });
          }
          // The content is what the model reads back as history; the report card
          // is what the user sees. Keep the content human so context is not blank.
          State.chatHistory.push({
            id: nextId++,
            role: "assistant",
            content: answer.text || answer.title,
            report: answer,
          });
        } else {
          // clarify or answer: a plain message, no chips.
          State.chatHistory.push({ id: nextId++, role: "assistant", content: answer.text });
        }
      } else {
        const reply = await Bridge.chatSend(query, context);
        State.chatHistory.push({ id: nextId++, role: "assistant", content: reply.text });
      }
      State.stateOverride = null;
      Sound.play("finish");
      const answer = [...State.chatHistory].reverse().find((m) => m.role === "assistant");
      void Bridge.log(`chat < ${dest} ${Date.now() - started}ms ${logText(answer?.content ?? "")}`);
    } catch (err) {
      State.stateOverride = null;
      State.noteMessage = String(err).replace(/^Error:\s*/, "");
      State.view = "note";
      Sound.play("error");
      void Bridge.log(`chat ! ${dest} ${logText(State.noteMessage)}`);
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
        for (const m of State.chatHistory) log.append(bubble(m, (v) => void submit(v)));
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
