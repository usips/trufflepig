"""Shared installer test scaffolding: isolated stores and service shims."""
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

PLUGIN = Path(__file__).resolve().parents[1]
UNIT_NAMES = ("trufflepig-system.service", "trufflepig-board.service")
CURRENT_ROUTER_STATUS = {
    "status": "ok", "board_api": 7, "schema_supported": 8, "schema_file": 8,
}


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

    def run_installer_main(self, *args, router_timeout=0.05):
        """Run the installer in-process with a short test-only router deadline."""
        from contextlib import redirect_stderr, redirect_stdout
        from unittest.mock import patch

        module = self.load_installer("install_agent_main_test")
        require_router = module.require_current_router

        def bounded_router_check(runtime, *, allow_absent=False, managed_restart=False, timeout=None):
            return require_router(runtime, allow_absent=allow_absent,
                                  managed_restart=managed_restart, timeout=router_timeout)

        module.require_current_router = bounded_router_check
        stdout = io.StringIO()
        stderr = io.StringIO()
        command = [str(PLUGIN / "install.sh"), *args]
        with patch.dict(os.environ, self.env), patch.object(sys, "argv", command):
            with redirect_stdout(stdout), redirect_stderr(stderr):
                try:
                    status = module.main()
                except (OSError, ValueError, subprocess.CalledProcessError) as error:
                    print(f"trufflepig-agent install: {error}", file=stderr)
                    for detail in getattr(error, "rollback_errors", ()):
                        print(f"systemd rollback: {detail}", file=stderr)
                    status = 2
        return subprocess.CompletedProcess(command, status, stdout.getvalue(), stderr.getvalue())

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
            shim.write_text((PLUGIN / "tests/agent_service_shim.py").read_text())
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

    def enabled_units(self):
        """Whether installed services have persistent or runtime enable links."""
        return {name: self.systemctl("is-enabled", name).stdout.strip()
                in ("enabled", "enabled-runtime")
                for name in UNIT_NAMES}

    def enabled_state(self, name):
        """Recorded enablement, including a unit removed during rollback."""
        state = json.loads(Path(self.env["SERVICE_STATE"]).read_text())
        return state.get("enabled", {}).get(name, "disabled")

    def set_enabled_state(self, name, status):
        """Seed systemctl's reported enablement for a preexisting unit."""
        state_path = Path(self.env["SERVICE_STATE"])
        state = json.loads(state_path.read_text()) if state_path.exists() else {}
        state.setdefault("enabled", {})[name] = status
        state_path.write_text(json.dumps(state, indent=2) + "\\n")

    def router_boots(self):
        state = Path(self.env["SERVICE_STATE"])
        if not state.exists():
            return 0
        return json.loads(state.read_text()).get("boots", {}).get("trufflepig-system.service", 0)

    def loaded_unit_hashes(self):
        """Definitions used by actual starts/restarts, including active no-op starts."""
        state = Path(self.env["SERVICE_STATE"])
        return json.loads(state.read_text()).get("loaded_hashes", {})

    def serve_router_status(self, replies, *, listen_delay=0.0, response_delay=0.0,
                            stall=False, raw_reply=None):
        """Answer `system status` polls on $TRUFFLEPIG_SYSTEM_DIR/daemon.sock.

        `replies` is a list consumed in order (the last entry repeats) or a
        callable evaluated per request. `raw_reply` bypasses the normal router
        response envelope for malformed-protocol regression cases.
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
        if not listen_delay:
            server.bind(str(path))
            server.listen(8)
            server.settimeout(0.05)
        stop = threading.Event()
        served = []

        def serve():
            try:
                if listen_delay:
                    if stop.wait(listen_delay):
                        return
                    server.bind(str(path))
                    server.listen(8)
                    server.settimeout(0.05)
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
                        if stall:
                            stop.wait()
                            break
                        if response_delay and stop.wait(response_delay):
                            break
                        if raw_reply is None:
                            payload = replies() if callable(replies) else replies[min(len(served), len(replies) - 1)]
                            served.append(payload)
                            reply = json.dumps({"status": "success", "output": json.dumps(payload)}).encode()
                        else:
                            reply = raw_reply if isinstance(raw_reply, bytes) else json.dumps(raw_reply).encode()
                        connection.sendall(len(reply).to_bytes(4, "big") + reply)
            finally:
                server.close()

        thread = threading.Thread(target=serve, daemon=True)
        thread.start()

        def stop_router():
            stop.set()
            thread.join(10)
            try:
                path.unlink()
            except FileNotFoundError:
                pass

        self.addCleanup(stop_router)
        return path

    def serve_router_upgrade(self, before, after):
        """Report `before` until the router unit boots again, then `after`."""
        baseline = self.router_boots()
        return self.serve_router_status(lambda: after if self.router_boots() > baseline else before)
