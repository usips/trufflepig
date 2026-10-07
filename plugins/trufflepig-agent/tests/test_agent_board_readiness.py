"""Installer router readiness and rollback contracts."""
import json
from pathlib import Path
import time
import unittest

from agent_install_case import CURRENT_ROUTER_STATUS, AgentInstallCase


class AgentBoardReadinessTests(AgentInstallCase):
    def test_router_failure_advice_matches_installer_restart_ownership(self):
        module = self.load_installer("install_router_advice")
        runtime = Path(self.env["TRUFFLEPIG_SYSTEM_DIR"])
        self.serve_router_status([{**CURRENT_ROUTER_STATUS, "board_api": 3}])
        for managed_restart in (False, True):
            with self.subTest(managed_restart=managed_restart):
                with self.assertRaises(ValueError) as caught:
                    options = {"managed_restart": True} if managed_restart else {}
                    module.require_current_router(runtime, timeout=0.05, **options)
                if managed_restart:
                    self.assertIn("rerun plugins/trufflepig-agent/install.sh --systemd",
                                  str(caught.exception))
                else:
                    self.assertIn("restart trufflepig-system.service with the current trufflepig binary",
                                  str(caught.exception))
                    self.assertNotIn("--systemd", str(caught.exception))

    def test_router_check_accepts_a_delayed_actual_status_reply(self):
        module = self.load_installer("install_router_delayed")
        runtime = Path(self.env["TRUFFLEPIG_SYSTEM_DIR"])
        self.serve_router_status([CURRENT_ROUTER_STATUS], listen_delay=0.03)
        started = time.monotonic()
        module.require_current_router(runtime, timeout=0.5)
        self.assertGreater(time.monotonic() - started, 0.015)
        self.assertLess(time.monotonic() - started, 0.5)

    def test_router_check_requires_managed_listener_but_allows_unmanaged_absence(self):
        module = self.load_installer("install_router_absent")
        runtime = Path(self.env["TRUFFLEPIG_SYSTEM_DIR"])
        started = time.monotonic()
        with self.assertRaisesRegex(ValueError, "did not become ready"):
            module.require_current_router(runtime, timeout=0.03)
        self.assertLess(time.monotonic() - started, 0.3)
        module.require_current_router(runtime, allow_absent=True, timeout=0.03)

    def test_unmanaged_preflight_rejects_stalled_and_malformed_present_routers_before_writes(self):
        capture = self.service_shims()
        malformed_replies = [
            ("list reply", []),
            ("scalar reply", 7),
            ("missing output", {"status": "success"}),
            ("non-string output", {"status": "success", "output": 7}),
            ("non-object status", {"status": "success", "output": "[]"}),
        ]
        cases = [("stalled", None, True), *[(name, reply, False) for name, reply in malformed_replies]]
        for index, (name, reply, stall) in enumerate(cases):
            with self.subTest(case=name):
                runtime = self.root / f"unmanaged-runtime-{index}"
                self.env["TRUFFLEPIG_SYSTEM_DIR"] = str(runtime)
                if stall:
                    self.serve_router_status([], stall=True)
                else:
                    self.serve_router_status([], raw_reply=reply)
                started = time.monotonic()
                result = self.run_installer_main("--board", router_timeout=0.04)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("router did not become ready", result.stderr)
                self.assertLess(time.monotonic() - started, 2.0)
                self.assertFalse((self.root / ".local/bin").exists())
                self.assertFalse((self.root / "config").exists())
                self.assertFalse((self.root / ".agents").exists())
                self.assertFalse((self.root / "config/systemd/user/trufflepig-board.service").exists())
                self.assertFalse(capture.exists())

    def test_failed_managed_readiness_restores_unit_files_and_active_states(self):
        capture = self.service_shims()
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        router_unit = units / "trufflepig-system.service"
        board_unit = units / "trufflepig-board.service"
        previous_router = "previous router unit\n"
        previous_board = "previous board unit\n"
        router_unit.write_text(previous_router)
        board_unit.write_text(previous_board)
        self.systemctl("enable", "trufflepig-system.service")
        self.systemctl("enable", "trufflepig-board.service")
        self.systemctl("start", "trufflepig-system.service")
        self.systemctl("start", "trufflepig-board.service")
        previous_hashes = self.loaded_unit_hashes()
        capture.write_text("")

        malformed_replies = [
            ("list reply", []),
            ("scalar reply", 7),
            ("missing output", {"status": "success"}),
            ("non-string output", {"status": "success", "output": 7}),
            ("non-object status", {"status": "success", "output": "[]"}),
        ]
        cases = [("absent listener", "absent", None),
                 ("partial schema restart", "status",
                  {**CURRENT_ROUTER_STATUS, "schema_file": CURRENT_ROUTER_STATUS["schema_supported"] - 1}),
                 *[(name, "raw", reply) for name, reply in malformed_replies]]
        for index, (name, kind, reply) in enumerate(cases):
            with self.subTest(case=name):
                self.env["TRUFFLEPIG_SYSTEM_DIR"] = str(self.root / f"managed-runtime-{index}")
                if kind == "status":
                    self.serve_router_status([reply])
                elif kind == "raw":
                    self.serve_router_status([], raw_reply=reply)
                started = time.monotonic()
                result = self.run_installer_main("--systemd", "--board", router_timeout=0.04)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("router", result.stderr)
                self.assertLess(time.monotonic() - started, 2.0)
                self.assertEqual(router_unit.read_text(), previous_router)
                self.assertEqual(board_unit.read_text(), previous_board)
                self.assertEqual(self.loaded_unit_hashes(), previous_hashes)
                self.assertEqual(self.active_units(),
                                 {"trufflepig-system.service": True, "trufflepig-board.service": True})
                self.assertEqual(self.enabled_units(),
                                 {"trufflepig-system.service": True, "trufflepig-board.service": True})
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        self.assertIn(["systemctl", "--user", "enable", "--now", "trufflepig-system.service"], calls)

    def test_failed_readiness_preserves_disabled_inactive_units(self):
        self.service_shims()
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        router_unit = units / "trufflepig-system.service"
        board_unit = units / "trufflepig-board.service"
        router_unit.write_text("previous router unit\n")
        board_unit.write_text("previous board unit\n")

        result = self.run_installer_main("--systemd", "--board", router_timeout=0.04)

        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(router_unit.read_text(), "previous router unit\n")
        self.assertEqual(board_unit.read_text(), "previous board unit\n")
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": False, "trufflepig-board.service": False})
        self.assertEqual(self.enabled_units(),
                         {"trufflepig-system.service": False, "trufflepig-board.service": False})

    def test_failed_readiness_preserves_runtime_enabled_active_units(self):
        self.service_shims()
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        router_unit = units / "trufflepig-system.service"
        board_unit = units / "trufflepig-board.service"
        router_unit.write_text("previous router unit\n")
        board_unit.write_text("previous board unit\n")
        for name in ("trufflepig-system.service", "trufflepig-board.service"):
            self.systemctl("enable", "--runtime", name)
            self.systemctl("start", name)

        result = self.run_installer_main("--systemd", "--board", router_timeout=0.04)

        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(router_unit.read_text(), "previous router unit\n")
        self.assertEqual(board_unit.read_text(), "previous board unit\n")
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})
        self.assertEqual(self.enabled_state("trufflepig-system.service"), "enabled-runtime")
        self.assertEqual(self.enabled_state("trufflepig-board.service"), "enabled-runtime")

    def test_failed_readiness_restores_static_router_enablement(self):
        self.service_shims()
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        router_unit = units / "trufflepig-system.service"
        previous_router = "# static unit\nprevious router unit\n"
        router_unit.write_text(previous_router)
        self.systemctl("start", "trufflepig-system.service")

        result = self.run_installer_main("--systemd", router_timeout=0.04)

        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(router_unit.read_text(), previous_router)
        self.assertEqual(self.systemctl("is-enabled", "trufflepig-system.service").stdout.strip(),
                         "static")
        self.assertEqual(self.active_units()["trufflepig-system.service"], True)


if __name__ == "__main__":
    unittest.main()
