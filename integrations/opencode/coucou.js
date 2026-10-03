// Coucou ⇄ opencode bridge.
//
// Copy this file to one of opencode's plugin directories:
//   ~/.config/opencode/plugins/coucou.js   (global, recommended)
//   .opencode/plugins/coucou.js            (project only)
//
// opencode loads it at startup. It forwards session and tool events to the
// Coucou island through the same relay Claude Code uses, tagging every payload
// with `coucou_agent: "opencode"` so opencode gets its own pill.
//
// Like the Claude Code hooks, it must never block opencode: every send is
// fire-and-forget and swallows its own errors.

import { existsSync } from "node:fs"
import { homedir, platform } from "node:os"
import { join } from "node:path"

const AGENT = "opencode"

/** The relay command prefix for this machine, or null when Coucou isn't installed. */
function relayCommand() {
  const home = homedir()
  if (platform() === "win32") {
    const local = process.env.LOCALAPPDATA ?? join(home, "AppData", "Local")
    const exe = join(local, "Coucou", "bin", "coucou-hook.exe")
    return existsSync(exe) ? [exe] : null
  }
  if (platform() === "darwin") {
    const candidates = [
      join(home, "Library", "Application Support", "NotchBuddy", "nb-hook"),
      join(home, "Library", "Containers", "fr.louisraille.Coucou", "Data", "nb-hook"),
    ]
    const found = candidates.find((p) => existsSync(p))
    return found ? ["/bin/sh", found] : null
  }
  const dataHome = process.env.XDG_DATA_HOME ?? join(home, ".local", "share")
  const relay = join(dataHome, "coucou", "bin", "coucou-hook")
  return existsSync(relay) ? [relay] : null
}

const command = relayCommand()

/** Sends one canonical hook event to the island. Never throws, never awaits. */
function send(event, payload) {
  if (!command) return
  try {
    const proc = Bun.spawn([...command, "--agent", AGENT, event], {
      stdin: "pipe",
      stdout: "ignore",
      stderr: "ignore",
    })
    proc.stdin.write(JSON.stringify({ hook_event_name: event, coucou_agent: AGENT, ...payload }))
    proc.stdin.end()
  } catch {
    // Coucou may be closed, paused or not installed — opencode must keep going.
  }
}

/** opencode exposes ids under a few shapes across versions; read them defensively. */
function sessionId(properties) {
  return (
    properties?.info?.id ??
    properties?.info?.sessionID ??
    properties?.sessionID ??
    properties?.session_id ??
    null
  )
}

/** opencode's session title, when the event carries one. */
function sessionTitle(properties) {
  return (
    properties?.info?.title ??
    properties?.info?.summary?.title ??
    properties?.title ??
    null
  )
}

export const CoucouPlugin = async ({ directory }) => {
  // Coucou pairs a pill with the exact session so "Continue" can resume it and
  // so two sessions in two folders never read as the same one. cwd and the id
  // therefore ride on every event, not just session.created.
  const base = { cwd: directory }
  return {
    event: async ({ event }) => {
      const properties = event?.properties ?? {}
      const id = sessionId(properties)
      const title = sessionTitle(properties)
      const withMeta = (payload) => {
        const out = { ...base, ...payload }
        if (id) out.session_id = id
        if (title) out.title = title
        return out
      }
      switch (event?.type) {
        case "session.created":
          send("SessionStart", withMeta({ session_id: id ?? undefined }))
          break
        case "message.updated":
          if (properties?.info?.role === "user") {
            send("UserPromptSubmit", withMeta({
              prompt: properties.info.summary?.title ?? "Working…",
            }))
          }
          break
        case "session.idle":
          send("Stop", withMeta({}))
          break
        case "session.error":
          send("StopFailure", withMeta({}))
          break
      }
    },
    "tool.execute.before": async (input) => {
      send("PreToolUse", {
        ...base,
        session_id: input?.sessionID ?? input?.session_id ?? undefined,
        tool_name: input?.tool,
        tool_input: input?.args,
      })
    },
    "tool.execute.after": async (input) => {
      send("PostToolUse", {
        ...base,
        session_id: input?.sessionID ?? input?.session_id ?? undefined,
      })
    },
  }
}
