#!/usr/bin/env sh
# Harness SessionStart/SessionEnd hook: records the harness session id for the
# session's working directory so trufflepig-agent can tag every call with it.
# Reads the hook JSON payload on stdin ({session_id, cwd, hook_event_name, ...}).
# Usage: session-start.sh <kimi|muse> [start|end]
set -eu
harness="${1:-unknown}"
phase="${2:-start}"
state="${XDG_STATE_HOME:-$HOME/.local/state}/trufflepig/agent-sessions/$harness"
payload="$(cat)"
session="$(printf '%s' "$payload" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("session_id") or d.get("sessionId") or "")' 2>/dev/null || true)"
cwd="$(printf '%s' "$payload" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("cwd") or "")' 2>/dev/null || true)"
[ -n "$cwd" ] || cwd="$PWD"
key="$(printf '%s' "$cwd" | python3 -c 'import hashlib,sys; print(hashlib.blake2s(sys.stdin.read().encode(), digest_size=6).hexdigest())')"
mkdir -p "$state"
if [ "$phase" = "end" ]; then
    rm -f "$state/$key"
    exit 0
fi
# Best-effort: pre-warm the per-user system daemon (sandboxed harnesses cannot
# spawn it themselves); never change the hook's exit code or add output.
if [ "$phase" = "start" ]; then
    command -v trufflepig >/dev/null 2>&1 && trufflepig system ensure >/dev/null 2>&1 || true
fi
[ -n "$session" ] || exit 0
printf '%s\n' "$session" > "$state/$key"
