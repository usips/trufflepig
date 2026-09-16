#!/usr/bin/env sh
# Install the trufflepig agent plugin for Kimi Code and Muse Code.
#
#   install.sh [--bin DIR] [--kimi] [--muse] [--kimi-hooks] [--systemd] [--project DIR ...]
#
# --bin DIR       where the wrapper commands go (default ~/.local/bin, must be on PATH)
# --kimi          symlink the skill into $KIMI_CODE_HOME/skills (default ~/.kimi-code/skills)
# --muse          `muse skills install` the skill into Muse's personal skill store
# --kimi-hooks    append kimi/hooks.toml to the Kimi config between marker comments
# --systemd       install and enable the trufflepig-system user service (the router)
# --project DIR   symlink the skill into DIR/.agents/skills (both harnesses discover it)
# With no selector flags, --kimi --muse are both attempted.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
bin="${HOME}/.local/bin"
kimi_home="${KIMI_CODE_HOME:-$HOME/.kimi-code}"
do_kimi=0; do_muse=0; do_hooks=0; do_systemd=0; projects=""
while [ $# -gt 0 ]; do
    case "$1" in
        --bin) bin="$2"; shift ;;
        --kimi) do_kimi=1 ;;
        --muse) do_muse=1 ;;
        --kimi-hooks) do_hooks=1 ;;
        --systemd) do_systemd=1 ;;
        --project) projects="$projects $2"; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done
if [ $do_kimi -eq 0 ] && [ $do_muse -eq 0 ] && [ -z "$projects" ]; then
    do_kimi=1; do_muse=1
fi

mkdir -p "$bin"
for tool in trufflepig-agent trufflepig-audit; do
    ln -sfn "$here/bin/$tool" "$bin/$tool"
    echo "linked $bin/$tool"
done
ln -sfn "$here/hooks/session-start.sh" "$bin/trufflepig-agent-session"
echo "linked $bin/trufflepig-agent-session"
ln -sfn "$here/hooks/steer-search.py" "$bin/trufflepig-agent-steer"
echo "linked $bin/trufflepig-agent-steer"
command -v trufflepig >/dev/null 2>&1 || echo "warning: trufflepig is not on PATH (cargo install --path . --locked --features semantic-cuda)" >&2

skill="$here/skills/trufflepig-code-search"
if [ $do_kimi -eq 1 ]; then
    mkdir -p "$kimi_home/skills"
    ln -sfn "$skill" "$kimi_home/skills/trufflepig-code-search"
    echo "kimi skill: $kimi_home/skills/trufflepig-code-search"
fi
if [ $do_hooks -eq 1 ]; then
    config="$kimi_home/config.toml"
    if grep -q '# trufflepig-agent hooks begin' "$config" 2>/dev/null; then
        # Replace the managed block in place so upgrades pick up new hooks.
        python3 - "$config" "$here/kimi/hooks.toml" <<'PY'
import re, sys
config, snippet = sys.argv[1], sys.argv[2]
text = open(config, encoding="utf-8").read()
block = "# trufflepig-agent hooks begin\n" + open(snippet, encoding="utf-8").read() + "# trufflepig-agent hooks end\n"
text = re.sub(r"# trufflepig-agent hooks begin\n.*?# trufflepig-agent hooks end\n", lambda _: block, text, count=1, flags=re.S)
open(config, "w", encoding="utf-8").write(text)
PY
        echo "kimi hooks refreshed in $config"
    else
        { printf '\n# trufflepig-agent hooks begin\n'; cat "$here/kimi/hooks.toml"; printf '# trufflepig-agent hooks end\n'; } >> "$config"
        echo "kimi hooks appended to $config"
    fi
fi
if [ $do_muse -eq 1 ]; then
    if command -v muse >/dev/null 2>&1; then
        muse skills install "$skill" --scope user --force --json && echo "muse skill installed"
    else
        echo "muse not found; skipped" >&2
    fi
fi
if [ $do_systemd -eq 1 ]; then
    if trufflepig_bin="$(command -v trufflepig 2>/dev/null)" && command -v systemctl >/dev/null 2>&1; then
        unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
        mkdir -p "$unit_dir"
        python3 - "$here/systemd/trufflepig-system.service" "$unit_dir/trufflepig-system.service" "$trufflepig_bin" <<'UNIT'
import sys
src, dst, exe = sys.argv[1:4]
text = open(src, encoding="utf-8").read().replace("@TRUFFLEPIG@", exe)
open(dst, "w", encoding="utf-8").write(text)
UNIT
        systemctl --user daemon-reload
        # A router started by hand holds the socket; hand it over to the service.
        trufflepig system stop >/dev/null 2>&1 || true
        systemctl --user enable --now trufflepig-system.service
        echo "systemd user service: $unit_dir/trufflepig-system.service"
    else
        echo "systemd or trufflepig not found; skipped the router service" >&2
    fi
fi
for project in $projects; do
    mkdir -p "$project/.agents/skills"
    ln -sfn "$skill" "$project/.agents/skills/trufflepig-code-search"
    echo "project skill: $project/.agents/skills/trufflepig-code-search"
done
echo "audit logs: ${XDG_STATE_HOME:-$HOME/.local/state}/trufflepig/agent-audit (summarize with trufflepig-audit)"
