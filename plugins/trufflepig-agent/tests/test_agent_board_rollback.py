"""Install rollback contracts: unit state and files after failures."""
import hashlib
import json
from pathlib import Path
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
        self.assertIn("rerun plugins/trufflepig-agent/install.sh --systemd", result.stderr)
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

    def test_rollback_restarts_active_units_on_restored_files(self):
        self.service_shims()
        self.serve_router_status([CURRENT_ROUTER_STATUS])
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
        previous_hashes = self.loaded_unit_hashes()
        self.env["FAIL_ENABLE"] = "1"
        self.env["FAIL_ENABLE_UNIT"] = "trufflepig-board.service"
        result = self.install("--systemd", "--board")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(router_unit.read_text(), "previous router unit\n")
        self.assertEqual(board_unit.read_text(), "previous board unit\n")
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})
        self.assertEqual(self.enabled_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})
        self.assertEqual(self.loaded_unit_hashes(), previous_hashes)

    def test_rollback_stops_new_units_after_restarting_existing_units(self):
        capture = self.service_shims()
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        router = units / "trufflepig-system.service"
        router.write_text("previous router unit\n")
        self.systemctl("enable", router.name)
        self.systemctl("start", router.name)
        previous_hashes = self.loaded_unit_hashes()
        capture.write_text("")
        self.env["FAIL_ENABLE"] = "1"
        result = self.install("--board")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse((units / "trufflepig-board.service").exists())
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": False})
        self.assertEqual(self.enabled_state("trufflepig-board.service"), "disabled")
        self.assertEqual(self.loaded_unit_hashes()[router.name], previous_hashes[router.name])
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        restored_restart = ["systemctl", "--user", "restart", router.name]
        cleanup = ["systemctl", "--user", "disable", "--now", "trufflepig-board.service"]
        self.assertLess(calls.index(restored_restart, calls.index(restored_restart) + 1),
                        calls.index(cleanup))

    def test_start_of_active_unit_preserves_its_loaded_definition(self):
        self.service_shims()
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        router = units / "trufflepig-system.service"
        router.write_text("old router\n")
        self.systemctl("start", router.name)
        loaded = self.loaded_unit_hashes()
        router.write_text("new router\n")
        self.systemctl("start", router.name)
        self.assertEqual(self.loaded_unit_hashes(), loaded)
        self.systemctl("restart", router.name)
        self.assertEqual(self.loaded_unit_hashes()[router.name],
                         hashlib.sha256(router.read_bytes()).hexdigest())

    def test_install_accepts_older_systemd_missing_units_without_stdout(self):
        capture = self.service_shims()
        self.env["OLD_SYSTEMD_MISSING_STDOUT"] = "1"
        result = self.install("--board")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in capture.read_text().splitlines()]
        self.assertFalse(any(call[2] == "is-enabled" for call in calls), calls)
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": False, "trufflepig-board.service": True})

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
