#!/usr/bin/env python3
import hashlib, json, os, sys, time
from pathlib import Path
name = Path(sys.argv[0]).name
api_output = os.environ["INSTALLER_BOARD_API"] + "\n"
if name == "trufflepig" and sys.argv[1:] == ["--board-api-version"]:
    time.sleep(float(os.environ.get("PROBE_DELAY", "0")))
    sys.stdout.write(os.environ.get("PROBE_STDOUT", api_output))
    sys.stderr.write(os.environ.get("PROBE_STDERR", ""))
    sys.exit(int(os.environ.get("PROBE_STATUS", "0")))
if name == "trufflepig" and sys.argv[1:] == ["system", "dir"]:
    if os.environ.get("PROBE_STDOUT", api_output) != api_output:
        # Older binaries predate `system dir` and answer with usage.
        sys.stderr.write("usage: trufflepig [--help] ...\n")
        sys.exit(2)
    override = os.environ.get("TRUFFLEPIG_SYSTEM_DIR")
    fallback = os.path.join(os.environ.get("HOME", "/nonexistent"), ".cache/trufflepig/system")
    sys.stdout.write((override or fallback) + "\n")
    sys.exit(0)
with Path(os.environ["SERVICE_CAPTURE"]).open("a") as handle:
    handle.write(json.dumps([name, *sys.argv[1:]]) + "\n")
if name != "systemctl":
    sys.exit(0)
# Active starts do nothing; actual boots record the loaded unit definition.
units_dir = Path(os.environ["XDG_CONFIG_HOME"]) / "systemd/user"
state_path = Path(os.environ["SERVICE_STATE"])
state = json.loads(state_path.read_text()) if state_path.exists() else {}
units_state = state.setdefault("units", {})
enabled_state = state.setdefault("enabled", {})
boots = state.setdefault("boots", {})
loaded_hashes = state.setdefault("loaded_hashes", {})
words = [word for word in sys.argv[1:] if not word.startswith("-")]
command, units = (words[0], words[1:]) if words else ("", [])
def unknown(unit, status):
    sys.stderr.write(f"Unit {unit} could not be found.\n")
    sys.exit(status)
if command == "is-active":
    if not (units_dir / units[0]).is_file() and units[0] not in units_state:
        unknown(units[0], 4)
    status = units_state.get(units[0], "inactive")
    if "--quiet" not in sys.argv:
        sys.stdout.write(status + "\n")
    sys.exit(0 if status == "active" else 3)
if command == "is-enabled":
    if not (units_dir / units[0]).is_file():
        if "--quiet" not in sys.argv and not os.environ.get("OLD_SYSTEMD_MISSING_STDOUT"):
            sys.stdout.write("not-found\n")
        sys.exit(1)
    status = enabled_state.get(units[0], "disabled")
    if status == "disabled" and (units_dir / units[0]).read_text().startswith("# static unit\n"):
        status = "static"
    if "--quiet" not in sys.argv:
        sys.stdout.write(status + "\n")
    sys.exit(0 if status in ("enabled", "enabled-runtime") else 1)
for unit in units:
    known_without_file = (command in ("stop", "disable")
                          and (unit in units_state or unit in enabled_state))
    if not (units_dir / unit).is_file() and not known_without_file:
        unknown(unit, 5 if command == "try-restart" else 4)

def boot(unit, restart=False):
    if not restart and units_state.get(unit) in ("active", "activating", "reloading"):
        return
    units_state[unit] = "active"
    boots[unit] = boots.get(unit, 0) + 1
    loaded_hashes[unit] = hashlib.sha256((units_dir / unit).read_bytes()).hexdigest()

failure = 0
if command == "stop":
    for unit in units:
        units_state[unit] = "inactive"
    if "trufflepig-system.service" in units:
        units_state["trufflepig-board.service"] = "inactive"
elif command == "disable":
    for unit in units:
        if "--runtime" in sys.argv:
            if enabled_state.get(unit) == "enabled-runtime":
                enabled_state[unit] = "disabled"
        else:
            enabled_state[unit] = "disabled"
        if "--now" in sys.argv:
            units_state[unit] = "inactive"
    if "--now" in sys.argv and "trufflepig-system.service" in units:
        units_state["trufflepig-board.service"] = "inactive"
elif command in ("start", "restart"):
    for unit in units:
        boot(unit, restart=command == "restart")
elif command == "enable":
    for unit in units:
        enabled_state[unit] = "enabled-runtime" if "--runtime" in sys.argv else "enabled"
        if "--now" in sys.argv:
            boot(unit)
    # Fail one activation after applying it, as a partial `enable --now` can.
    if ("--now" in sys.argv and os.environ.get("FAIL_ENABLE")
            and os.environ.get("FAIL_ENABLE_UNIT", units[0]) in units
            and not state.get("fail_enable_consumed")):
        state["fail_enable_consumed"] = True
        failure = 1
elif command == "try-restart":
    for unit in units:
        if units_state.get(unit) in ("active", "activating", "reloading"):
            boot(unit, restart=True)
state_path.write_text(json.dumps(state, indent=2) + "\n")
sys.exit(failure)
