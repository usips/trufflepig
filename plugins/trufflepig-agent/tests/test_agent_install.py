"""Installer contracts with isolated personal stores and no live services."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

PLUGIN = Path(__file__).resolve().parents[1]


class AgentInstallTests(unittest.TestCase):
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

    def test_personal_and_quoted_project_installs_are_idempotent(self):
        project = self.root / "project with spaces"
        for _ in range(2):
            result = self.install("--codex", "--project", str(project))
            self.assertEqual(result.returncode, 0, result.stderr)
        skill = PLUGIN / "skills/trufflepig-code-search"
        self.assertEqual((self.root / ".agents/skills/trufflepig-code-search").resolve(), skill)
        self.assertEqual((project / ".agents/skills/trufflepig-code-search").resolve(), skill)
        config = json.loads((self.root / "config/trufflepig/agent-runtime.json").read_text())
        self.assertEqual(config["spool_dir"], str(self.root / ".cache/codex-tmp/trufflepig-agent/spool"))
        # The wrapper's sibling module must remain importable through its symlink.
        result = subprocess.run([str(self.root / ".local/bin/trufflepig-agent"), "--help"],
                                env=dict(self.env, TRUFFLEPIG_BINARY="/nonexistent"), capture_output=True)
        self.assertEqual(result.returncode, 127, result.stderr)
        self.assertNotIn(b"ModuleNotFoundError", result.stderr)

    def test_unmanaged_skill_prevents_partial_installation(self):
        skill = self.root / ".agents/skills/trufflepig-code-search"
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text("user content")
        result = self.install("--codex")
        self.assertEqual(result.returncode, 2)
        self.assertIn("unmanaged destination", result.stderr)
        self.assertEqual((skill / "SKILL.md").read_text(), "user content")
        self.assertFalse((self.root / ".local/bin").exists())

    def test_grok_install_respects_home_and_reuses_skill(self):
        grok = self.root / "grok home"
        self.env["GROK_HOME"] = str(grok)
        for _ in range(2):
            result = self.install("--grok")
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((grok / "skills/trufflepig-code-search").resolve(),
                         PLUGIN / "skills/trufflepig-code-search")
        self.assertFalse((self.root / ".agents").exists())
        self.assertFalse((grok / "config.toml").exists())
        config = json.loads((self.root / "config/trufflepig/agent-runtime.json").read_text())
        self.assertTrue(Path(config["runtime_dir"]).is_dir())

    def test_runtime_directory_rejects_ram_tmp(self):
        result = self.install("--codex", "--runtime-dir", "/tmp/trufflepig-test")
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.root / ".local/bin").exists())

    def test_service_quotes_paths_and_replaces_children(self):
        spec = importlib.util.spec_from_file_location("install_agent", PLUGIN / "scripts/install_agent.py")
        module = importlib.util.module_from_spec(spec)
        sys.path.insert(0, str(PLUGIN / "scripts"))
        self.addCleanup(sys.path.remove, str(PLUGIN / "scripts"))
        spec.loader.exec_module(module)
        from unittest.mock import patch
        canned = subprocess.CompletedProcess(args=[], returncode=0, stdout="/system-runtime\n", stderr="")
        with patch.object(module.subprocess, "run", return_value=canned):
            text = module.service_text('/home/a path/100%/trufflepig', Path('/home/a path/spool'))
        self.assertIn('ExecStart="/home/a path/100%%/trufflepig" system-serve', text)
        self.assertIn('Environment="TRUFFLEPIG_SPOOL_DIR=/home/a path/spool"', text)
        self.assertIn('KillMode=control-group', text)

    def test_board_service_is_foreground_and_shares_router_paths(self):
        spec = importlib.util.spec_from_file_location("install_board_service", PLUGIN / "scripts/install_agent.py")
        module = importlib.util.module_from_spec(spec)
        sys.path.insert(0, str(PLUGIN / "scripts"))
        self.addCleanup(sys.path.remove, str(PLUGIN / "scripts"))
        spec.loader.exec_module(module)
        from unittest.mock import patch
        canned = subprocess.CompletedProcess(args=[], returncode=0,
                                             stdout=f"{self.root}/runtime with spaces/100%\n", stderr="")
        with patch.dict(os.environ, {"HOME": str(self.root),
                                     "TRUFFLEPIG_BOARD_DB": str(self.root / "data with spaces/100%/board.sqlite3"),
                                     "TRUFFLEPIG_SYSTEM_DIR": str(self.root / "runtime with spaces/100%")}, clear=True), \
                patch.object(module.subprocess, "run", return_value=canned):
            board = module.render_service("trufflepig-board.service", "/bin with spaces/100%/trufflepig")
            router = module.service_text("/bin with spaces/100%/trufflepig", self.root / "spool")
        self.assertIn('ExecStart="/bin with spaces/100%%/trufflepig" board-serve --listen 127.0.0.1:0', board)
        self.assertIn("Type=simple", board)
        self.assertIn("Restart=on-failure", board)
        self.assertIn("WantedBy=default.target", board)
        for line in ("After=trufflepig-system.service", "Wants=trufflepig-system.service",
                     "PartOf=trufflepig-system.service"):
            self.assertIn(line, board)
        for name in ("TRUFFLEPIG_BOARD_DB", "TRUFFLEPIG_SYSTEM_DIR"):
            line = next(line for line in board.splitlines() if line.startswith(f'Environment="{name}='))
            self.assertIn(line, router)
            self.assertIn("100%%", line)
        self.assertNotIn("TRUFFLEPIG_SPOOL_DIR", board)
        self.assertFalse((self.root / "runtime with spaces").exists(), "unit generation must not create a token")

    def test_rendered_unit_pins_binary_resolved_runtime_dir(self):
        spec = importlib.util.spec_from_file_location("install_system_dir_threading", PLUGIN / "scripts/install_agent.py")
        module = importlib.util.module_from_spec(spec)
        sys.path.insert(0, str(PLUGIN / "scripts"))
        self.addCleanup(sys.path.remove, str(PLUGIN / "scripts"))
        spec.loader.exec_module(module)
        from unittest.mock import patch
        canned = Path("/canned/trufflepig/system")
        completed = subprocess.CompletedProcess(args=["/bin/trufflepig", "system", "dir"],
                                                returncode=0, stdout=f"{canned}\n", stderr="")
        with patch.object(module.subprocess, "run", return_value=completed) as run:
            text = module.service_text("/bin/trufflepig", None)
        run.assert_called_once_with(["/bin/trufflepig", "system", "dir"], stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, timeout=5)
        self.assertIn(f'Environment="TRUFFLEPIG_SYSTEM_DIR={canned}"', text)

    def test_board_install_selector_is_explicit_in_help(self):
        result = self.install("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--board", result.stdout)
        self.assertFalse((self.root / ".local/bin").exists())

    def service_shims(self):
        directory = self.root / "service-bin"
        directory.mkdir()
        capture = self.root / "service-calls.jsonl"
        for name in ("trufflepig", "systemctl"):
            shim = directory / name
            shim.write_text('''#!/usr/bin/env python3
import json, os, sys, time
from pathlib import Path
if Path(sys.argv[0]).name == "trufflepig" and sys.argv[1:] == ["--board-api-version"]:
    time.sleep(float(os.environ.get("PROBE_DELAY", "0")))
    sys.stdout.write(os.environ.get("PROBE_STDOUT", "5\\n"))
    sys.stderr.write(os.environ.get("PROBE_STDERR", ""))
    sys.exit(int(os.environ.get("PROBE_STATUS", "0")))
if Path(sys.argv[0]).name == "trufflepig" and sys.argv[1:] == ["system", "dir"]:
    override = os.environ.get("TRUFFLEPIG_SYSTEM_DIR")
    fallback = os.path.join(os.environ.get("HOME", "/nonexistent"), ".cache/trufflepig/system")
    sys.stdout.write((override or fallback) + "\\n")
    sys.exit(0)
with Path(os.environ["SERVICE_CAPTURE"]).open("a") as handle:
    handle.write(json.dumps([Path(sys.argv[0]).name, *sys.argv[1:]]) + "\\n")
''')
            shim.chmod(0o755)
        self.env.update(PATH=f"{directory}:{self.env['PATH']}", SERVICE_CAPTURE=str(capture))
        return capture

    def serve_router_status(self, replies):
        """Answer `system status` polls on $TRUFFLEPIG_SYSTEM_DIR/daemon.sock."""
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
                        payload = replies[min(len(served), len(replies) - 1)]
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

    def test_service_capability_failure_prevents_all_installation_mutations(self):
        capture = self.service_shims()
        cases = [("", "unknown argument --board-api-version\\n", "2"),
                 ("1\n", "", "0"), ("2\n", "", "0"), ("3\n", "", "0"), ("4\n", "", "0"),
                 ("API 5\n", "", "0"), ("5\nextra\n", "", "0"), ("5\n", "", "1"),
                 ("5\n", "diagnostic\n", "0")]
        for flag in ("--board", "--systemd"):
            for output, error, status in cases:
                with self.subTest(flag=flag, output=output, status=status):
                    self.env.update(PROBE_STDOUT=output, PROBE_STDERR=error, PROBE_STATUS=status)
                    result = self.install("--codex", flag)
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("must support board API 5", result.stderr)
                    self.assertFalse((self.root / ".local/bin").exists())
                    self.assertFalse((self.root / ".agents").exists())
                    self.assertFalse((self.root / "config").exists())
                    self.assertFalse((self.root / "system-runtime").exists())
                    self.assertFalse(capture.exists())

    def test_service_capability_timeout_prevents_mutations(self):
        capture = self.service_shims()
        self.env["PROBE_DELAY"] = "30"
        result = subprocess.run([str(PLUGIN / "install.sh"), "--board"], env=self.env,
                                text=True, capture_output=True, timeout=8)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("must support board API 5", result.stderr)
        self.assertFalse((self.root / ".local/bin").exists())
        self.assertFalse((self.root / "config").exists())
        self.assertFalse(capture.exists())

    def test_service_preflight_rejects_relative_board_path_before_mutation(self):
        capture = self.service_shims()
        self.env["TRUFFLEPIG_BOARD_DB"] = "relative.sqlite3"
        for flag in ("--board", "--systemd"):
            with self.subTest(flag=flag):
                result = self.install(flag)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("TRUFFLEPIG_BOARD_DB must be absolute", result.stderr)
                self.assertFalse((self.root / ".local/bin").exists())
                self.assertFalse((self.root / "config/systemd").exists())
                self.assertFalse(capture.exists())

    def test_board_only_installs_no_router_and_combined_reloads_once(self):
        capture = self.service_shims()
        result = self.install("--board")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        self.assertEqual(calls, [["systemctl", "--user", "daemon-reload"],
                                 ["systemctl", "--user", "stop", "trufflepig-board.service"],
                                 ["systemctl", "--user", "enable", "--now", "trufflepig-board.service"]])
        units = self.root / "config/systemd/user"
        self.assertTrue((units / "trufflepig-board.service").is_file())
        self.assertFalse((units / "trufflepig-system.service").exists())
        capture.write_text("")
        result = self.install("--systemd", "--board")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        self.assertEqual(calls.count(["systemctl", "--user", "daemon-reload"]), 1)
        self.assertIn(["systemctl", "--user", "enable", "--now", "trufflepig-system.service"], calls)
        self.assertIn(["systemctl", "--user", "enable", "--now", "trufflepig-board.service"], calls)

    def test_later_board_install_restarts_the_installed_router(self):
        capture = self.service_shims()
        first = self.install("--systemd")
        self.assertEqual(first.returncode, 0, first.stderr)
        capture.write_text("")
        result = self.install("--board")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        self.assertEqual(calls, [["systemctl", "--user", "daemon-reload"],
                                 ["systemctl", "--user", "stop", "trufflepig-board.service"],
                                 ["systemctl", "--user", "restart", "trufflepig-system.service"],
                                 ["systemctl", "--user", "enable", "--now", "trufflepig-board.service"]])

    def test_systemd_install_try_restarts_board_after_router_enable(self):
        capture = self.service_shims()
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        (units / "trufflepig-board.service").write_text("previous board unit\n")
        result = self.install("--systemd")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        enable = ["systemctl", "--user", "enable", "--now", "trufflepig-system.service"]
        self.assertIn(enable, calls)
        self.assertEqual(calls[calls.index(enable) + 1],
                         ["systemctl", "--user", "try-restart", "trufflepig-board.service"])

    def test_systemd_install_without_board_skips_try_restart(self):
        capture = self.service_shims()
        result = self.install("--systemd")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        self.assertNotIn(["systemctl", "--user", "try-restart", "trufflepig-board.service"], calls)

    def test_failing_router_check_restores_previous_unit_files(self):
        capture = self.service_shims()
        current = {"board_api": 5, "schema_supported": "2026-01-01", "schema_file": "2026-01-01"}
        stale = {"board_api": 3, "schema_supported": "2026-01-01", "schema_file": "2026-01-01"}
        self.serve_router_status([current, stale])
        first = self.install("--systemd", "--board")
        self.assertEqual(first.returncode, 0, first.stderr)
        units = self.root / "config/systemd/user"
        board_unit = units / "trufflepig-board.service"
        previous = "previous board unit\n"
        board_unit.write_text(previous)
        capture.write_text("")
        result = self.install("--board")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("board_api 3", result.stderr)
        self.assertEqual(board_unit.read_text(), previous)
        reload = ["systemctl", "--user", "daemon-reload"]
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        self.assertEqual(calls.count(reload), 2)
        self.assertEqual(calls[-1], reload)
        self.assertNotIn(["systemctl", "--user", "enable", "--now", "trufflepig-board.service"], calls)
        board_unit.unlink()
        capture.write_text("")
        result = self.install("--board")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse(board_unit.exists())
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        self.assertEqual(calls.count(reload), 2)
        self.assertEqual(calls[-1], reload)

    def test_router_check_polls_until_router_answers(self):
        spec = importlib.util.spec_from_file_location("install_router_poll", PLUGIN / "scripts/install_agent.py")
        module = importlib.util.module_from_spec(spec)
        sys.path.insert(0, str(PLUGIN / "scripts"))
        self.addCleanup(sys.path.remove, str(PLUGIN / "scripts"))
        spec.loader.exec_module(module)
        from unittest.mock import patch
        current = {"board_api": 5, "schema_supported": "s", "schema_file": "s"}
        with patch.object(module, "router_status", side_effect=[None, None, current]) as status, \
                patch.object(module.time, "sleep") as sleep:
            module.require_current_router(self.root / "runtime")
        self.assertEqual(status.call_count, 3)
        self.assertEqual(sleep.call_count, 2)

    def test_router_check_returns_when_no_router_answers_within_timeout(self):
        spec = importlib.util.spec_from_file_location("install_router_timeout", PLUGIN / "scripts/install_agent.py")
        module = importlib.util.module_from_spec(spec)
        sys.path.insert(0, str(PLUGIN / "scripts"))
        self.addCleanup(sys.path.remove, str(PLUGIN / "scripts"))
        spec.loader.exec_module(module)
        from unittest.mock import patch
        with patch.object(module, "router_status", return_value=None) as status, \
                patch.object(module.time, "monotonic", side_effect=[100.0, 200.0]), \
                patch.object(module.time, "sleep") as sleep:
            module.require_current_router(self.root / "runtime")
        self.assertEqual(status.call_count, 1)
        sleep.assert_not_called()

    def test_later_board_install_rejects_different_router_database_before_mutation(self):
        capture = self.service_shims()
        runtime = self.root / "router-runtime"
        pinned = self.root / "custom data/100%/board.sqlite3"
        self.env.update(TRUFFLEPIG_SYSTEM_DIR=str(runtime), TRUFFLEPIG_BOARD_DB=str(pinned),
                        XDG_DATA_HOME=str(self.root / "default-data"))
        first = self.install("--systemd")
        self.assertEqual(first.returncode, 0, first.stderr)
        unit = self.root / "config/systemd/user/trufflepig-system.service"
        previous = unit.read_text()
        capture.write_text("")
        for marker in (False, True):
            if marker:
                runtime.mkdir(mode=0o700)
                (runtime / "board-backend.json").write_text(json.dumps({"database": str(pinned)}))
            for candidate in (None, str(self.root / "other/board.sqlite3")):
                if candidate is None:
                    self.env.pop("TRUFFLEPIG_BOARD_DB", None)
                else:
                    self.env["TRUFFLEPIG_BOARD_DB"] = candidate
                with self.subTest(marker=marker, candidate=candidate):
                    result = self.install("--board")
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("board database mismatch", result.stderr)
                    self.assertIn(f"set TRUFFLEPIG_BOARD_DB={pinned}", result.stderr)
                    self.assertEqual(unit.read_text(), previous)
                    self.assertFalse(unit.with_name("trufflepig-board.service").exists())
                    self.assertEqual(capture.read_text(), "")

    def test_board_service_pin_accepts_canonical_database_alias(self):
        self.service_shims()
        pinned = self.root / "existing.sqlite3"
        pinned.touch()
        alias = self.root / "alias.sqlite3"
        alias.symlink_to(pinned)
        runtime = self.root / "runtime"
        runtime.mkdir()
        (runtime / "board-backend.json").write_text(json.dumps({"database": str(pinned)}))
        self.env.update(TRUFFLEPIG_SYSTEM_DIR=str(runtime), TRUFFLEPIG_BOARD_DB=str(alias))
        result = self.install("--board")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_later_board_install_rejects_changed_router_runtime(self):
        capture = self.service_shims()
        runtime = self.root / "custom-runtime"
        self.env["TRUFFLEPIG_SYSTEM_DIR"] = str(runtime)
        first = self.install("--systemd")
        self.assertEqual(first.returncode, 0, first.stderr)
        capture.write_text("")
        del self.env["TRUFFLEPIG_SYSTEM_DIR"]
        self.env["XDG_RUNTIME_DIR"] = str(self.root / "different-runtime")
        result = self.install("--board")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(f"set TRUFFLEPIG_SYSTEM_DIR={runtime}", result.stderr)
        self.assertFalse((self.root / "config/systemd/user/trufflepig-board.service").exists())
        self.assertEqual(capture.read_text(), "")

    def test_router_unit_unsets_apply_after_later_environment_assignments(self):
        self.service_shims()
        unit = self.root / "config/systemd/user/trufflepig-system.service"
        unit.parent.mkdir(parents=True)
        unit.write_text('[Service]\nUnsetEnvironment = TRUFFLEPIG_BOARD_DB\n'
                        'Environment = "TRUFFLEPIG_BOARD_DB=/different.sqlite3"\n')
        result = self.install("--board")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_router_drop_in_whitespace_cannot_hide_database_or_environment_file(self):
        capture = self.service_shims()
        first = self.install("--systemd")
        self.assertEqual(first.returncode, 0, first.stderr)
        capture.write_text("")
        drop_in = self.root / "config/systemd/user/trufflepig-system.service.d/override.conf"
        drop_in.parent.mkdir()
        for directive, message in [('Environment = "TRUFFLEPIG_BOARD_DB=/different.sqlite3"', 'set TRUFFLEPIG_BOARD_DB=/different.sqlite3'),
                                   ('EnvironmentFile = /unknown.env', 'cannot validate board service EnvironmentFile')]:
            drop_in.write_text('[Service]\n' + directive + '\n')
            result = self.install("--board")
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn(message, result.stderr)
            self.assertFalse((self.root / "config/systemd/user/trufflepig-board.service").exists())
            self.assertEqual(capture.read_text(), "")

    def test_check_reports_broken_runtime(self):
        fake = self.root / "failing wrapper"
        fake.write_text('#!/bin/sh\necho "daemon: unexpected argument --format" >&2\nexit 2\n')
        fake.chmod(0o755)
        result = subprocess.run([sys.executable, str(PLUGIN / "scripts/check_agent.py"),
                                 "--wrapper", str(fake), str(self.root)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("member daemons", result.stderr)


if __name__ == "__main__":
    unittest.main()
