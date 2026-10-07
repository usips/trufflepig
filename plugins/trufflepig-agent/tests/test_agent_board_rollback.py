"""Install rollback contracts: unit state and files after failures."""
import json
from pathlib import Path
import time
import unittest

from agent_install_case import CURRENT_ROUTER_STATUS, AgentInstallCase


class AgentBoardRollbackTests(AgentInstallCase):
    def test_failing_router_check_leaves_units_running_and_files_untouched(self):
        self.service_shims()
        current = CURRENT_ROUTER_STATUS
        stale = {"status": "ok", "board_api": 3, "schema_supported": 8, "schema_file": 8}
        self.serve_router_status([current, stale])
        first = self.install("--systemd", "--board")
        self.assertEqual(first.returncode, 0, first.stderr)
        units = self.root / "config/systemd/user"
        board_unit = units / "trufflepig-board.service"
        previous = "previous board unit\n"
        board_unit.write_text(previous)
        self.systemctl("start", "trufflepig-system.service")
        self.systemctl("start", "trufflepig-board.service")
        result = self.install("--board")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("board_api 3", result.stderr)
        self.assertEqual(board_unit.read_text(), previous)
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})
        self.assertEqual(self.enabled_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})
        board_unit.unlink()
        result = self.install("--board")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse(board_unit.exists())
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": False})
        self.assertEqual(self.enabled_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": False})

    def test_enable_failure_restores_files_and_restarts_active_units(self):
        self.service_shims()
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        router_unit = units / "trufflepig-system.service"
        board_unit = units / "trufflepig-board.service"
        router_unit.write_text("previous router unit\n")
        board_unit.write_text("previous board unit\n")
        self.systemctl("enable", "trufflepig-system.service")
        self.systemctl("enable", "trufflepig-board.service")
        self.systemctl("start", "trufflepig-system.service")
        self.systemctl("start", "trufflepig-board.service")
        self.env["FAIL_ENABLE"] = "1"
        result = self.install("--systemd")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(router_unit.read_text(), "previous router unit\n")
        self.assertEqual(board_unit.read_text(), "previous board unit\n")
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})
        self.assertEqual(self.enabled_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})

    def test_failed_board_enable_leaves_no_new_units(self):
        self.service_shims()
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        (units / "trufflepig-system.service").write_text("previous router unit\n")
        self.env["FAIL_ENABLE"] = "1"
        result = self.install("--board")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse((units / "trufflepig-board.service").exists())
        self.assertEqual((units / "trufflepig-system.service").read_text(), "previous router unit\n")
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": False, "trufflepig-board.service": False})
        self.assertEqual(self.enabled_units(),
                         {"trufflepig-system.service": False, "trufflepig-board.service": False})
        self.assertEqual(self.enabled_state("trufflepig-board.service"), "disabled")

    def test_failed_board_enable_restores_disabled_inactive_existing_units(self):
        self.service_shims()
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        router_unit = units / "trufflepig-system.service"
        board_unit = units / "trufflepig-board.service"
        router_unit.write_text("previous router unit\n")
        board_unit.write_text("previous board unit\n")
        self.env["FAIL_ENABLE"] = "1"

        result = self.install("--board")

        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(router_unit.read_text(), "previous router unit\n")
        self.assertEqual(board_unit.read_text(), "previous board unit\n")
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": False, "trufflepig-board.service": False})
        self.assertEqual(self.enabled_units(),
                         {"trufflepig-system.service": False, "trufflepig-board.service": False})

    def test_crash_looping_board_counts_as_active(self):
        self.service_shims()
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        (units / "trufflepig-board.service").write_text("crash looping board\n")
        state = Path(self.env["SERVICE_STATE"])
        state.write_text(json.dumps({"units": {"trufflepig-board.service": "activating"}}))
        result = self.install("--systemd")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})

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
        capture.write_text("")

        malformed_replies = [
            ("list reply", []),
            ("scalar reply", 7),
            ("missing output", {"status": "success"}),
            ("non-string output", {"status": "success", "output": 7}),
            ("non-object status", {"status": "success", "output": "[]"}),
        ]
        cases = [("absent listener", "absent", None),
                 ("partial schema restart", "status", {**CURRENT_ROUTER_STATUS, "schema_file": 7}),
                 *[(name, "raw", reply) for name, reply in malformed_replies]]
        for index, (name, kind, reply) in enumerate(cases):
            with self.subTest(case=name):
                self.env["TRUFFLEPIG_SYSTEM_DIR"] = str(self.root / f"managed-runtime-{index}")
                if kind == "status":
                    self.serve_router_status([reply])
                elif kind == "raw":
                    self.serve_router_status([], raw_reply=reply)
                boots_before = self.router_boots()
                started = time.monotonic()
                result = self.run_installer_main("--systemd", "--board", router_timeout=0.04)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("router", result.stderr)
                self.assertLess(time.monotonic() - started, 2.0)
                self.assertEqual(router_unit.read_text(), previous_router)
                self.assertEqual(board_unit.read_text(), previous_board)
                self.assertEqual(self.router_boots(), boots_before + 2)
                self.assertEqual(self.active_units(),
                                 {"trufflepig-system.service": True, "trufflepig-board.service": True})
                self.assertEqual(self.enabled_units(),
                                 {"trufflepig-system.service": True, "trufflepig-board.service": True})
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        self.assertIn(["systemctl", "--user", "enable", "--now", "trufflepig-system.service"], calls)

    def test_missing_schema_requires_a_matching_absent_database_without_board_error(self):
        self.service_shims()
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        router_unit = units / "trufflepig-system.service"
        board_unit = units / "trufflepig-board.service"
        previous_router = "previous router unit\n"
        previous_board = "previous board unit\n"
        router_unit.write_text(previous_router)
        board_unit.write_text(previous_board)
        database = Path(self.env["TRUFFLEPIG_BOARD_DB"])
        current_reply = {"value": None}
        self.serve_router_status(lambda: current_reply["value"])

        missing_database = self.fresh_router_status()
        missing_database.pop("board_db")
        cases = [
            ("missing database path", missing_database, None),
            ("relative database path", self.fresh_router_status(board_db="board.sqlite3"), None),
            ("non-string database path", self.fresh_router_status(board_db=7), None),
            ("mismatched database path",
             self.fresh_router_status(board_db=str(database.with_name("other.sqlite3"))), None),
            ("reported board error",
             self.fresh_router_status(board_error="database pin could not be validated"), None),
            ("existing file with missing schema", self.fresh_router_status(), "file"),
            ("database directory with missing schema", self.fresh_router_status(), "directory"),
            ("dangling database symlink with missing schema",
             self.fresh_router_status(), "dangling-symlink"),
        ]
        for name, status, database_kind in cases:
            with self.subTest(case=name):
                if database.is_symlink() or database.is_file():
                    database.unlink()
                elif database.is_dir():
                    database.rmdir()
                current_reply["value"] = status
                if database_kind:
                    database.parent.mkdir(parents=True, exist_ok=True)
                    if database_kind == "file":
                        database.touch()
                    elif database_kind == "directory":
                        database.mkdir()
                    elif database_kind == "dangling-symlink":
                        database.symlink_to(database.with_name("missing-target.sqlite3"))

                result = self.run_installer_main("--systemd", "--board", router_timeout=0.04)

                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("router", result.stderr)
                self.assertEqual(router_unit.read_text(), previous_router)
                self.assertEqual(board_unit.read_text(), previous_board)
                self.assertEqual(self.active_units(),
                                 {"trufflepig-system.service": False, "trufflepig-board.service": False})
                self.assertEqual(self.enabled_units(),
                                 {"trufflepig-system.service": False, "trufflepig-board.service": False})
                if database_kind == "file":
                    self.assertTrue(database.is_file())
                elif database_kind == "directory":
                    self.assertTrue(database.is_dir())
                elif database_kind == "dangling-symlink":
                    self.assertTrue(database.is_symlink())

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

    def test_masked_enablement_states_fail_before_unit_mutations(self):
        self.service_shims()
        cases = [
            ("masked", "--systemd", "trufflepig-system.service"),
            ("masked-runtime", "--systemd", "trufflepig-system.service"),
            ("masked", "--board", "trufflepig-board.service"),
            ("masked-runtime", "--board", "trufflepig-board.service"),
        ]
        for index, (state_name, flag, target) in enumerate(cases):
            with self.subTest(state=state_name, flag=flag):
                config_home = self.root / f"masked-config-{index}"
                unit_dir = config_home / "systemd/user"
                unit_dir.mkdir(parents=True)
                router_unit = unit_dir / "trufflepig-system.service"
                board_unit = unit_dir / "trufflepig-board.service"
                previous_router = "previous router unit\n"
                previous_board = "previous board unit\n"
                router_unit.write_text(previous_router)
                board_unit.write_text(previous_board)
                state_path = self.root / f"masked-state-{index}.json"
                state_path.write_text(json.dumps({"enabled": {target: state_name}}))
                capture = self.root / f"masked-calls-{index}.jsonl"
                self.env.update(XDG_CONFIG_HOME=str(config_home), SERVICE_STATE=str(state_path),
                                SERVICE_CAPTURE=str(capture))

                result = self.run_installer_main(flag, router_timeout=0.04)

                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn(f"systemd reports {state_name}", result.stderr)
                self.assertEqual(router_unit.read_text(), previous_router)
                self.assertEqual(board_unit.read_text(), previous_board)
                calls = [json.loads(line) for line in capture.read_text().splitlines()]
                self.assertTrue(calls)
                self.assertFalse(any(call[2] not in ("is-active", "is-enabled") for call in calls),
                                 calls)


if __name__ == "__main__":
    unittest.main()
