#!/usr/bin/env sh
# Install shared Codex, Claude, Kimi, and Muse integration; --help lists selectors.
set -eu
here="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
exec python3 "$here/scripts/install_agent.py" "$@"
