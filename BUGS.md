# Bug log

Every bug, misleading state or wrong label I see goes here, fixed or not.

Rules:
- One entry per bug. `file:line` is mandatory so it can be jumped to.
- Status is `fixed` or `open`. Nothing is closed without a check that proves it.
- A bug that is *misleading* counts as a bug: wrong label, switch that shows the
  wrong state, text that lies about what the app does.
- Duplicate credential/field lists are bugs too: they drift, and the drift ships.

Status of this file: `windows/` front end unless stated otherwise.

## Fixed — 2026-10-04

| # | Bug | Where | Fix |
|---|-----|-------|-----|
| F1 | ERP card showed raw metadata (`12 entity sets · 8 types · all report fields present`). Noise, not actionable. | `windows/src/views/integrations.ts` `sapStatus` | Plain copy: `Connected` / `Live data ready — ask about orders, stock or partners.` |
| F2 | `toggleIntegration` wrote `integration.<id>` from the array *before* mutation, so the flag lagged one click: switching a pill on did nothing, off stayed on. | `windows/src/core/state.ts:451` | Hoisted `wasActive`; flag is now `!wasActive`. |
| F3 | Appearance → Pills shown → Opencode wrote `pill.integration_opencode`. No such pill; the id is `agent_opencode`. The opencode pill could never be hidden. | `windows/src/settings/main.ts:737` | Id corrected. |
| F4 | Integrations page switch read `features[key] !== false`, island reads `State.integrationOn` which falls back to `activeIntegrations`. Stripe, Notion and Cal.com showed "on" while their pill was off. | `windows/src/settings/main.ts:664` | `features[key] ?? activeIntegrations.includes(id)`. |
| F5 | Switch labelled `Pill` controlled polling, not visibility, and the page hint promised two switches that did not exist. | `windows/src/settings/main.ts:504,695` | Relabelled `Active`; hint points at Appearance → Pills shown. |
| F6 | Cal.com pill asked the Credential Manager for `calcom-token`; Settings writes `calcom-api-key`. Pill always said "needs credentials" after a successful save. | `windows/src/island/integrations.ts:18` | Key corrected. |
| F7 | ERP card cached key presence for the whole process. Saving credentials in Settings left the card saying "Needs credentials" until restart. | `windows/src/views/integrations.ts`, `windows/src/main.ts` | `invalidateSapKeys()` on `settings-changed`. |

## Open

| # | Bug | Where | Repro | Suggested fix |
|---|-----|-------|-------|---------------|
| O1 | No in-island way to switch a pill off. `TOGGLEABLE_INTEGRATION_IDS` and `toggleIntegration` are never called from any view. | `windows/src/core/state.ts:106,451` | Look for a toggle on any pill: absent. | Either wire the pill context menu to `toggleIntegration`, or delete both as dead code. |
| O2 | `integrationOn` hardcodes `integration_claude` and `integration_sapb1` as always on. Those two can never be switched off, and no switch exists for them. | `windows/src/core/state.ts:293` | Turn off Claude Code anywhere in Settings: impossible. | Drop the hardcode, give both a switch, or state in the UI that they are always on. |
| O3 | n8n "configured" check only looks at `n8n-api-key` and ignores `n8n-url`, so the pill claims to be configured with no instance URL. | `windows/src/island/integrations.ts:16` | Save only the API key. | Move n8n to `KEYS_FOR` with both keys, like the ERP does. |
| O4 | Appearance per-pill switch folds in the master row: with "Show pill row" off every pill switch reads off, but a click stores `true` and nothing visibly changes. | `windows/src/settings/main.ts:742` | Turn master row off, then click a pill switch. | Read the flag alone; grey the list out while the master row is off. |
| O5 | Settings window listens to `settings-changed` but never re-renders, so any write from the island leaves the page stale. | `windows/src/settings/main.ts:831` | Toggle a pill in Settings, change something in the island, come back. | Re-render the current page in the listener. |
| O6 | Credential field lists are duplicated three times: `SAP_FIELDS`, the Settings `fields` array, `KEY_FOR`/`KEYS_FOR`. This duplication caused F6 and can cause more. | `windows/src/views/integrations.ts:529`, `windows/src/settings/main.ts:597`, `windows/src/island/integrations.ts:11` | — | One exported table in `core/bridge.ts` keyed by pill id; all three read it. |
| O7 | Settings → Integrations → Test connection still prints entity set and type counts. | `windows/src/settings/main.ts:641` | Test a live connection. | Reuse the card's plain wording. |
| O8 | `const agents` block in `agentsPage` is indented at column 0 inside the function. | `windows/src/settings/main.ts:139` | Open Settings → Agents. | Cosmetic: re-indent. |
| O9 | Hiding a pill leaves no trace in the island, and no hint that anything was switched off. Turning a service off and its pill away look identical from the notch. | — | Hide every pill. | Consider one "hidden" placeholder or a Settings-only summary. |

## Seen but not judged yet

Nothing yet. Add rows here when a finding needs a decision before it becomes a bug.
