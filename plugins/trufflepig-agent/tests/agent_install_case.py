"""Shared installer test scaffolding: isolated stores and service shims."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

PLUGIN = Path(__file__).resolve().parents[1]
UNIT_NAMES = ("trufflepig-system.service", "trufflepig-board.service")


class AgentInstallCase(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="trufflepig-install-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.env = dict({k: v for k, v in os.environ.items()
                         if not k.startswith("TRUFFLEPIG") and k not in ("CLAUDE_CONFIG_DIR", "GROK_HOME", "XDG_RUNTIME_DIR")},
                        HOME=str(self.root), XDG_CONFIG_HOME=str(self.root / "config"), XDG_STATE_HOME=str(self.root / "state"),
                        TRUFFLEPIG_SYSTEM_DIR=str(self.root / "system-runtime"), TRUFFLEPIG_BOARD_DB=str(self.root / "data/board.sqlite3"))

    def install(self, *args):
        return subprocess.run([str(PLUGIN / "install.sh"), *args], env=self.env, text=True, capture_output=True)

    def load_installer(self, name):
        """Import scripts/install_agent.py as a fresh module named `name`."""
        spec = importlib.util.spec_from_file_location(name, PLUGIN / "scripts/install_agent.py")
        module = importlib.util.module_from_spec(spec)
        sys.path.insert(0, str(PLUGIN / "scripts"))
        self.addCleanup(sys.path.remove, str(PLUGIN / "scripts"))
        spec.loader.exec_module(module)
        return module

    def service_shims(self):
        directory = self.root / "service-bin"
        directory.mkdir()
        capture = self.root / "service-calls.jsonl"
        state = self.root / "service-state.json"
        for name in ("trufflepig", "systemctl"):
            shim = directory / name
            shim.write_text('''#!/usr/bin/env python3
import json, os, sys, time
from pathlib import Path
name = Path(sys.argv[0]).name
if name == "trufflepig" and sys.argv[1:] == ["--board-api-version"]:
    time.sleep(float(os.environ.get("PROBE_DELAY", "0")))
    sys.stdout.write(os.environ.get("PROBE_STDOUT", "5\\n"))
    sys.stderr.write(os.environ.get("PROBE_STDERR", ""))
    sys.exit(int(os.environ.get("PROBE_STATUS", "0")))
if name == "trufflepig" and sys.argv[1:] == ["system", "dir"]:
    if os.environ.get("PROBE_STDOUT", "5\\n") != "5\\n":
        # Older binaries predate `system dir` and answer with usage.
        sys.stderr.write("usage: trufflepig [--help] ...\\n")
        sys.exit(2)
    override = os.environ.get("TRUFFLEPIG_SYSTEM_DIR")
    fallback = os.path.join(os.environ.get("HOME", "/nonexistent"), ".cache/trufflepig/system")
    sys.stdout.write((override or fallback) + "\\n")
    sys.exit(0)
with Path(os.environ["SERVICE_CAPTURE"]).open("a") as handle:
    handle.write(json.dumps([name, *sys.argv[1:]]) + "\\n")
if name != "systemctl":
    sys.exit(0)
# Model the unit state systemctl would keep: is-active prints the unit
# state and exits nonzero unless it is "active", the board's PartOf stops
# it with the router, try-restart revives running units only, and a unit
# is known only while its unit file exists. Starts count boots so tests
# can tell a restarted router apart from one still running old code.
units_dir = Path(os.environ["XDG_CONFIG_HOME"]) / "systemd/user"
state_path = Path(os.environ["SERVICE_STATE"])
state = json.loads(state_path.read_text()) if state_path.exists() else {}
units_state = state.setdefault("units", {})
boots = state.setdefault("boots", {})
words = [word for word in sys.argv[1:] if not word.startswith("-")]
command, units = (words[0], words[1:]) if words else ("", [])
def unknown(unit, status):
    sys.stderr.write(f"Unit {unit} could not be found.\\n")
    sys.exit(status)
if command == "is-active":
    if not (units_dir / units[0]).is_file():
        unknown(units[0], 4)
    status = units_state.get(units[0], "inactive")
    if "--quiet" not in sys.argv:
        sys.stdout.write(status + "\\n")
    sys.exit(0 if status == "active" else 3)
for unit in units:
    if not (units_dir / unit).is_file():
        unknown(unit, 5 if command == "try-restart" else 4)
failure = 0
if command == "stop" or (command == "disable" and "--now" in sys.argv):
    for unit in units:
        units_state[unit] = "inactive"
    if "trufflepig-system.service" in units:
        units_state["trufflepig-board.service"] = "inactive"
elif command in ("start", "restart"):
    for unit in units:
        units_state[unit] = "active"
        boots[unit] = boots.get(unit, 0) + 1
elif command == "enable" and "--now" in sys.argv:
    # FAIL_ENABLE leaves the unit running, as a partial `enable --now` can.
    for unit in units:
        units_state[unit] = "active"
        boots[unit] = boots.get(unit, 0) + 1
    if os.environ.get("FAIL_ENABLE"):
        failure = 1
elif command == "try-restart":
    # try-restart revives running units only: a running unit restarts in
    # place and a stopped unit stays stopped, so end state never changes.
    pass
state_path.write_text(json.dumps(state, indent=2) + "\\n")
sys.exit(failure)
''')
            shim.chmod(0o755)
        self.env.update(PATH=f"{directory}:{self.env['PATH']}",
                        SERVICE_CAPTURE=str(capture), SERVICE_STATE=str(state))
        return capture

    def systemctl(self, *args):
        return subprocess.run(["systemctl", "--user", *args], env=self.env, text=True, capture_output=True)

    def active_units(self):
        """Unit states per the systemctl shim: active includes activating."""
        live = ("active", "activating", "reloading")
        return {name: self.systemctl("is-active", name).stdout.strip() in live
                for name in UNIT_NAMES}

    def router_boots(self):
        state = Path(self.env["SERVICE_STATE"])
        if not state.exists():
            return 0
        return json.loads(state.read_text()).get("boots", {}).get("trufflepig-system.service", 0)

    def serve_router_status(self, replies):
        """Answer `system status` polls on $TRUFFLEPIG_SYSTEM_DIR/daemon.sock.

        `replies` is a list consumed in order (the last entry repeats) or a
        callable evaluated per request.
        """
        import socket
        import threading
        runtime = Path(self.env["TRUFFLEPIG_SYSTEM_DIR"])
        runtime.mkdir(parents=True, exist_ok=True)
        path = runtime / "daemon.sock"
        try:
            path.unlink()
        except FileNotFoundError:
            pass
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(str(path))
        server.listen(8)
        server.settimeout(5)
        stop = threading.Event()
        served = []

        def serve():
            try:
                while not stop.is_set():
                    try:
                        connection, _ = server.accept()
                    except socket.timeout:
                        continue
                    with connection:
                        connection.settimeout(5)
                        raw = connection.recv(4)
                        if len(raw) < 4:
                            continue
                        length = int.from_bytes(raw, "big")
                        body = b""
                        while len(body) < length:
                            chunk = connection.recv(length - len(body))
                            if not chunk:
                                break
                            body += chunk
                        payload = replies() if callable(replies) else replies[min(len(served), len(replies) - 1)]
                        served.append(payload)
                        reply = json.dumps({"status": "success", "output": json.dumps(payload)}).encode()
                        connection.sendall(len(reply).to_bytes(4, "big") + reply)
            finally:
                server.close()

        thread = threading.Thread(target=serve, daemon=True)
        thread.start()

        def stop_router():
            stop.set()
            thread.join(10)

        self.addCleanup(stop_router)
        return path

    def serve_router_upgrade(self, before, after):
        """Report `before` until the router unit boots again, then `after`."""
        baseline = self.router_boots()
        return self.serve_router_status(lambda: after if self.router_boots() > baseline else before)
