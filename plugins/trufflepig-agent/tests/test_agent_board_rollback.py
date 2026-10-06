"""Install rollback contracts: unit state and files after failures."""
import json
from pathlib import Path
import unittest
from unittest.mock import patch

from agent_install_case import AgentInstallCase


class AgentBoardRollbackTests(AgentInstallCase):
    def test_failing_router_check_leaves_units_running_and_files_untouched(self):
        self.service_shims()
        current = {"board_api": 5, "schema_supported": "2026-01-01", "schema_file": "2026-01-01"}
        stale = {"board_api": 3, "schema_supported": "2026-01-01", "schema_file": "2026-01-01"}
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
        board_unit.unlink()
        result = self.install("--board")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse(board_unit.exists())
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": False})

    def test_enable_failure_restores_files_and_restarts_active_units(self):
        self.service_shims()
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        router_unit = units / "trufflepig-system.service"
        board_unit = units / "trufflepig-board.service"
        router_unit.write_text("previous router unit\n")
        board_unit.write_text("previous board unit\n")
        self.systemctl("start", "trufflepig-system.service")
        self.systemctl("start", "trufflepig-board.service")
        self.env["FAIL_ENABLE"] = "1"
        result = self.install("--systemd")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(router_unit.read_text(), "previous router unit\n")
        self.assertEqual(board_unit.read_text(), "previous board unit\n")
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})

    def test_failed_board_enable_leaves_no_new_units(self):
        self.service_shims()
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

    def test_crash_looping_board_counts_as_active(self):
        self.service_shims()
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        (units / "trufflepig-board.service").write_text("crash looping board\n")
        state = Path(self.env["SERVICE_STATE"])
        state.write_text(json.dumps({"units": {"trufflepig-board.service": "activating"}}))
        result = self.install("--systemd")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.active_units(),
                         {"trufflepig-system.service": True, "trufflepig-board.service": True})

    def test_router_check_polls_until_router_answers(self):
        module = self.load_installer("install_router_poll")
        current = {"board_api": 5, "schema_supported": "s", "schema_file": "s"}
        with patch.object(module, "router_status", side_effect=[None, None, current]) as status, \
                patch.object(module.time, "sleep") as sleep:
            module.require_current_router(self.root / "runtime")
        self.assertEqual(status.call_count, 3)
        self.assertEqual(sleep.call_count, 2)

    def test_router_check_returns_when_no_router_answers_within_timeout(self):
        module = self.load_installer("install_router_timeout")
        with patch.object(module, "router_status", return_value=None) as status, \
                patch.object(module.time, "monotonic", side_effect=[100.0, 200.0]), \
                patch.object(module.time, "sleep") as sleep:
            module.require_current_router(self.root / "runtime")
        self.assertEqual(status.call_count, 1)
        sleep.assert_not_called()


if __name__ == "__main__":
    unittest.main()
