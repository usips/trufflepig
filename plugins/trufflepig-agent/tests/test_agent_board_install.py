"""Service install behavior: capability gates and unit state after installs."""
import json
from pathlib import Path
import subprocess
import unittest

from agent_install_case import CURRENT_ROUTER_STATUS, PLUGIN, AgentInstallCase

STALE_ROUTER = {**CURRENT_ROUTER_STATUS, "board_api": CURRENT_ROUTER_STATUS["board_api"] - 1}


class AgentBoardInstallTests(AgentInstallCase):
    def test_service_capability_failure_prevents_all_installation_mutations(self):
        capture = self.service_shims()
        api = CURRENT_ROUTER_STATUS["board_api"]
        cases = [("", "unknown argument --board-api-version\\n", "2"),
                 *[(f"{version}\n", "", "0") for version in range(1, api)],
                 (f"API {api}\n", "", "0"), (f"{api}\nextra\n", "", "0"),
                 (f"{api}\n", "", "1"), (f"{api}\n", "diagnostic\n", "0")]
        for flag in ("--board", "--systemd"):
            for output, error, status in cases:
                with self.subTest(flag=flag, output=output, status=status):
                    self.env.update(PROBE_STDOUT=output, PROBE_STDERR=error, PROBE_STATUS=status)
                    result = self.install("--codex", flag)
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn(f"must support board API {api}", result.stderr)
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
        self.assertIn(f"must support board API {CURRENT_ROUTER_STATUS['board_api']}", result.stderr)
        self.assertFalse((self.root / ".local/bin").exists())
        self.assertFalse((self.root / "config").exists())
        self.assertFalse(capture.exists())

    def test_api_3_binary_gets_reinstall_advice_not_usage(self):
        self.service_shims()
        self.env["PROBE_STDOUT"] = "3\n"
        for flag in ("--systemd", "--board"):
            with self.subTest(flag=flag):
                result = self.install(flag)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn(f"must support board API {CURRENT_ROUTER_STATUS['board_api']}", result.stderr)
                self.assertNotIn("usage", result.stderr)
                self.assertFalse((self.root / "config/systemd").exists())

    def test_board_only_installs_no_router_and_combined_reloads_once(self):
        capture = self.service_shims()
        result = self.install("--board")
        self.assertEqual(result.returncode, 0, result.stderr)
        units = self.root / "config/systemd/user"
        self.assertTrue((units / "trufflepig-board.service").is_file())
        self.assertFalse((units / "trufflepig-system.service").exists())
        self.assertEqual(self.enabled_units(),
                         {"trufflepig-system.service": False, "trufflepig-board.service": True})
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": False, "trufflepig-board.service": True})
        capture.write_text("")
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        result = self.install("--systemd", "--board")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        self.assertEqual(calls.count(["systemctl", "--user", "daemon-reload"]), 1)
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})

    def test_later_board_install_restarts_the_installed_router(self):
        self.service_shims()
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        first = self.install("--systemd")
        self.assertEqual(first.returncode, 0, first.stderr)
        boots = self.router_boots()
        result = self.install("--board")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.router_boots(), boots + 1)
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})

    def test_board_install_upgrades_a_stale_router(self):
        self.service_shims()
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        (units / "trufflepig-system.service").write_text("stale router unit\n")
        self.systemctl("start", "trufflepig-system.service")
        self.serve_router_upgrade(STALE_ROUTER, CURRENT_ROUTER_STATUS)
        result = self.install("--board")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.router_boots(), 2)
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})

    def test_systemd_board_install_upgrades_a_stale_router(self):
        self.service_shims()
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        (units / "trufflepig-system.service").write_text("stale router unit\n")
        self.systemctl("start", "trufflepig-system.service")
        self.serve_router_upgrade(STALE_ROUTER, CURRENT_ROUTER_STATUS)
        result = self.install("--systemd", "--board")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.router_boots(), 2)
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})

    def test_systemd_alone_with_active_board_leaves_both_active(self):
        capture = self.service_shims()
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        (units / "trufflepig-board.service").write_text("previous board unit\n")
        self.systemctl("start", "trufflepig-board.service")
        self.systemctl("disable", "trufflepig-board.service")
        capture.write_text("")
        result = self.install("--systemd")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        enable = ["systemctl", "--user", "enable", "--now", "trufflepig-system.service"]
        self.assertIn(enable, calls)
        self.assertEqual(calls[calls.index(enable) + 1],
                         ["systemctl", "--user", "start", "trufflepig-board.service"])
        self.assertEqual(self.enabled_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": False})
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})

    def test_systemd_install_without_board_skips_board_start(self):
        self.service_shims()
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        result = self.install("--systemd")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / "config/systemd/user/trufflepig-board.service").exists())
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": False})


if __name__ == "__main__":
    unittest.main()
