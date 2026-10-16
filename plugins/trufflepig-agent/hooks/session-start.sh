#!/usr/bin/env sh
# Harness SessionStart/SessionEnd hook: best-effort start of the per-user system
# daemon. It writes no state; actor identity comes from the harness's native
# session variable, TRUFFLEPIG_SESSION, or a call's --session claim.
# Usage: session-start.sh <kimi|muse> [start|end]  (the harness name is informational)
set -eu
phase="${2:-start}"
# Drain the hook's JSON payload from stdin; its fields are not needed.
cat >/dev/null
# Best-effort: make sure the per-user system daemon runs (sandboxed harnesses
# cannot spawn it themselves). Prefer the systemd user service installed by
# `install.sh --systemd`, which also restarts the router after a crash; fall
# back to a detached spawn. Never change the hook's exit code or add output.
if [ "$phase" = "start" ]; then
    if command -v systemctl >/dev/null 2>&1 && systemctl --user is-enabled trufflepig-system.service >/dev/null 2>&1; then
        systemctl --user start trufflepig-system.service >/dev/null 2>&1 || true
    else
        command -v trufflepig >/dev/null 2>&1 && trufflepig system ensure >/dev/null 2>&1 || true
    fi
fi
