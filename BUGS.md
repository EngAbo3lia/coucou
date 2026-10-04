# Bug log

Every bug, misleading state or wrong label I see goes here, fixed or not.

Rules:
- One entry per bug. `file:line` is mandatory so it can be jumped to.
- Status is `fixed` or `open`. Nothing is closed without a check that proves it.
- A bug that is *misleading* counts as a bug: wrong label, switch that shows the
  wrong state, text that lies about what the app does.
- Duplicate credential/field lists are bugs too: they drift, and the drift ships.

Scope: `windows/` front end unless stated otherwise. Verified with
`npx tsc --noEmit` (clean) and `cargo check` (clean; 32 pre-existing warnings in
unrelated SAP modules) in `windows/`.

> Testing build only: chat turns are written to `coucou.log` in full (F17). That
> is deliberate for now and must be removed before release. Every TEMP marker is
> in the source; grep `TEMP (test` to find all of them.

## Fixed — 2026-10-04

| # | Bug | Where | Fix |
|---|-----|-------|-----|
| F1 | ERP card showed raw metadata (`12 entity sets · 8 types · all report fields present`). Noise, not actionable. | `windows/src/views/integrations.ts` `sapStatus` | Plain copy: `Connected` / `Live data ready — ask about orders, stock or partners.` |
| F2 | `toggleIntegration` wrote `integration.<id>` from the array *before* mutation, so the flag lagged one click: switching a pill on did nothing, off stayed on. | `windows/src/core/state.ts` | Hoisted `wasActive`; flag is now `!wasActive`. (Method removed later as dead code — see F19.) |
| F3 | Appearance → Pills shown → Opencode wrote `pill.integration_opencode`. No such pill; the id is `agent_opencode`. The opencode pill could never be hidden. | `windows/src/settings/main.ts` | Id corrected. |
| F4 | Integrations page switch read `features[key] !== false`, island reads `State.integrationOn` which falls back to `activeIntegrations`. Stripe, Notion and Cal.com showed "on" while their pill was off. | `windows/src/settings/main.ts` `loadPresence` | `features[key] ?? activeIntegrations.includes(id)`. |
| F5 | Switch labelled `Pill` controlled polling, not visibility, and the page hint promised two switches that did not exist. | `windows/src/settings/main.ts` | Relabelled `Active`; hint points at Appearance → Pills shown. |
| F6 | Cal.com pill asked the Credential Manager for `calcom-token`; Settings writes `calcom-api-key`. Pill always said "needs credentials" after a successful save. | `windows/src/island/integrations.ts` | Key corrected. |
| F7 | ERP card cached key presence for the whole process. Saving credentials in Settings left the card saying "Needs credentials" until restart. | `windows/src/views/integrations.ts`, `windows/src/main.ts` | `invalidateSapKeys()` on `settings-changed`. |
| F8 | Credential field lists existed in four places (`SAP_FIELDS`, Settings `fields`, `OTHER_INTEGRATIONS`, island `KEY_FOR`). This duplication *is* F6 and can repeat. | `windows/src/core/bridge.ts` | One `CREDENTIAL_FIELDS` table keyed by pill id; ERP card, Settings form and island all read it. |
| F9 | n8n counted as configured with only `n8n-api-key`; the instance URL was never checked, so the pill claimed ready with no URL. | `windows/src/island/integrations.ts` | `refreshConfigured` now requires every key of the pill. |
| F10 | `loadIntegrationTasks` re-hardcoded Claude Code and SAP Harness as always on, so a stored `integration.<id> = false` was ignored and those two could never be switched off. | `windows/src/core/state.ts` `loadIntegrationTasks` | Removed the hardcode; a stored flag is honoured. Added `Active` switches for both in Settings. |
| F11 | Settings → SAP → Test connection still printed entity set and type counts. | `windows/src/settings/main.ts` `sapSection` | `Connected — live data ready.` |
| F12 | Settings window listened to `settings-changed` but never repainted, so any write from the island left the open page stale. | `windows/src/settings/main.ts` | `render()` in the listener. |
| F13 | Appearance per-pill switch folded in the master row: with "Show pill row" off every pill switch read off, but a click stored `true` and nothing changed. | `windows/src/settings/main.ts` `appearancePage` | Reads the pill flag alone. |
| F14 | `const agents` in `agentsPage` sat at column 0 inside the function. | `windows/src/settings/main.ts` | Re-indented. |
| F15 | `TOGGLEABLE_INTEGRATION_IDS` was exported and never referenced anywhere in the repo. | `windows/src/core/state.ts` | Removed. |
| F16 | Chat turns left no trace in the log. | `windows/src/views/chat.ts` `submit` | Metadata lines added: `chat > <dest>`, `chat < <dest> ok <ms>`, `chat ! <dest>`. |
| F17 | TEMP: the metadata-only logging (F16) hid the actual conversation, which is exactly what is needed while testing. | `windows/src/views/chat.ts` | Full question, answer and error now written to `coucou.log`, one flattened line each, capped at 2000 chars. Grep `TEMP (test`; remove before release. |
| F18 | The log path was in `windows/README.md` but nowhere in the app, with no way to open it. | `windows/src-tauri/src/lib.rs`, `windows/src/core/bridge.ts`, `windows/src/settings/main.ts` | `log_path` and `open_log_folder` commands; Settings → Advanced → Diagnostics shows the path and has an "Open log folder" button. |
| F19 | `toggleIntegration` had no caller on Windows (macOS calls it from `SettingsView.swift:933`; the Windows port inlines its own switch). | `windows/src/core/state.ts` | Removed as dead code. |
| F20 | Nothing told the user that hiding a pill does not stop its service; the island shrinks to a single pill while pollers keep running. | `windows/src/settings/main.ts` `appearancePage` | Appearance hint now states hidden pills keep polling and points at the Integrations switch. |
| F21 | A hidden pill left no trace in the island at all: no way to see that a pill had been switched off, from the notch. | `windows/src/views/views.ts` `buildSettings` | The in-island Settings row now shows an amber `N hidden` badge when any pill is off. Switching pills back on stays in the full Settings window — the island card is too small for a list, and its rows must not be restyled. |

## Open

Nothing open.

## Seen but not judged yet

Nothing yet. Add rows here when a finding needs a decision before it becomes a bug.
