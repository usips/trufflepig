#!/usr/bin/env sh
# Install shared skills, commands, and optional router/board services; --help lists selectors.
set -eu
here="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
exec python3 "$here/scripts/install_agent.py" "$@"
